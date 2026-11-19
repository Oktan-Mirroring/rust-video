//! Submit-slot ring: pooled command buffers plus completion tracking.
//!
//! [`SubmitSlots`] owns the pool, one command buffer plus fence per
//! slot, and the pending-completion collection behind them.

use std::sync::{Arc, Mutex};

use crate::error::VulkanError;
use crate::inflight::{Fence, Inflight};
use crate::raw::{CommandPool, VkFence};

use ash::vk;
use rust_video_caps::delivery::SubmitStats;

/// Retries of one submit after a reset, before the error surfaces.
pub(crate) const SUBMIT_RETRIES: u32 = 1;

/// Submit slots per session: one records while earlier ones complete.
pub(crate) const SUBMIT_SLOTS: usize = 3;

/// Completion token of one submitted slot.
struct SlotFence {
    /// Ring index behind the token.
    slot: usize,
    /// Slot fence, shared with the ring.
    fence: Arc<VkFence>,
}

impl Fence for SlotFence {
    fn signaled(&self) -> Result<bool, VulkanError> {
        self.fence.signaled()
    }

    fn wait(&self, timeout_ns: u64) -> Result<bool, VulkanError> {
        self.fence.wait(timeout_ns)
    }
}

/// One pooled command buffer plus its completion fence.
struct SubmitSlot {
    /// Recorded command buffer.
    cmd: vk::CommandBuffer,
    /// Completion fence, shared with pending tokens.
    fence: Arc<VkFence>,
    /// Submitted without collection.
    busy: bool,
}

/// Pooled submit slots over one queue plus completion tracking.
///
/// Submit and poll split: `submit` queues without waiting, `poll`
/// waits outside the queue lock. Every fallible method leaves the
/// ring reset on error; the caller then resets its own session state,
/// since tracked layouts and reference bookkeeping no longer match
/// the device. A failed submit retries once after the reset; a
/// failed poll cannot retry, its completion is unknown.
pub(crate) struct SubmitSlots {
    /// Logical device behind idle waits.
    device: ash::Device,
    /// Video queue.
    queue: vk::Queue,
    /// Queue submission mutex, shared with the device.
    queue_lock: Arc<Mutex<()>>,
    /// Command pool behind the slots; drops with the ring.
    pool: CommandPool,
    /// Ring slots.
    slots: Vec<SubmitSlot>,
    /// Submitted fences not yet reaped.
    inflight: Inflight<SlotFence>,
    /// Next acquire candidate.
    next: usize,
    /// Successful queue submits since open.
    submitted: u64,
    /// Deepest submitted-but-uncollected count observed.
    peak: usize,
    /// Ring resets after failures.
    resets: u64,
    /// Driver objects created since open.
    creates: u64,
    /// Idle wait failed; the next op retries the reset first.
    poisoned: bool,
    /// Submits to fail before running, armed by tests.
    fail_next: u32,
}

impl SubmitSlots {
    /// Ring over `queue` with `SUBMIT_SLOTS` slots.
    ///
    /// # Errors
    ///
    /// Returns `Err` when pool, command buffer, or fence creation fails.
    pub(crate) fn open(
        device: &ash::Device,
        family: u32,
        queue: vk::Queue,
        queue_lock: Arc<Mutex<()>>,
    ) -> Result<Self, VulkanError> {
        let pool = CommandPool::create(device, family)?;
        let mut slots = Vec::with_capacity(SUBMIT_SLOTS);
        let mut creates = 1;
        for _ in 0..SUBMIT_SLOTS {
            slots.push(SubmitSlot {
                cmd: pool.allocate()?,
                fence: Arc::new(VkFence::new(device, false)?),
                busy: false,
            });
            creates += 2;
        }
        Ok(Self {
            device: device.clone(),
            queue,
            queue_lock,
            pool,
            slots,
            inflight: Inflight::new(),
            next: 0,
            submitted: 0,
            peak: 0,
            resets: 0,
            creates,
            poisoned: false,
            fail_next: 0,
        })
    }

    /// Command pool behind the slots.
    #[must_use]
    pub(crate) fn pool(&self) -> &CommandPool {
        &self.pool
    }

    /// Command buffer of `index`.
    #[must_use]
    pub(crate) fn cmd(&self, index: usize) -> vk::CommandBuffer {
        self.slots[index].cmd
    }

    /// Gauges snapshot.
    #[must_use]
    pub(crate) fn stats(&self) -> SubmitStats {
        SubmitStats::snapshot(self.submitted, self.peak, self.resets, self.creates)
    }

    /// Submits since the last take, zeroing the counter.
    pub(crate) fn take_submitted(&mut self) -> u64 {
        std::mem::replace(&mut self.submitted, 0)
    }

    /// Resets since the last take, zeroing the counter.
    pub(crate) fn take_resets(&mut self) -> u64 {
        std::mem::replace(&mut self.resets, 0)
    }

    /// Driver creates since the last take, zeroing the counter.
    pub(crate) fn take_creates(&mut self) -> u64 {
        std::mem::replace(&mut self.creates, 0)
    }

    /// Fail the next submit once, exercising the reset path.
    pub(crate) fn inject_fault(&mut self) {
        self.fail_next += 1;
    }

    /// Free slot index, polling a busy one when the ring fills.
    ///
    /// The fence resets for the next submit. A full ring waits on one
    /// busy slot outside the queue lock, which back-pressures the
    /// producer to the ring depth.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the reset or the back-pressure wait fails.
    pub(crate) fn acquire(&mut self) -> Result<usize, VulkanError> {
        if self.poisoned {
            self.reset_ring()?;
        }
        self.free_completed()?;
        if let Some(index) = self.free_from_next() {
            return self.take(index);
        }
        self.poll(self.next)?;
        self.take(self.next)
    }

    /// Queue-submit `index` without waiting, retrying once after a reset.
    ///
    /// Tracks the slot fence for completion and records the inflight
    /// peak. The retry reuses the recording; a failed submit never
    /// reaches the queue.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the submit fails after its retry, or the
    /// reset between attempts fails.
    pub(crate) fn submit(&mut self, index: usize) -> Result<(), VulkanError> {
        if self.poisoned {
            self.reset_ring()?;
        }
        let mut attempt = 0;
        loop {
            match self.submit_once(index) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if attempt >= SUBMIT_RETRIES {
                        return Err(error);
                    }
                    attempt += 1;
                    eprintln!(
                        "vulkan: submit failed ({error}); resetting, retry {attempt} of {SUBMIT_RETRIES}"
                    );
                    self.reset_ring()?;
                }
            }
        }
    }

    /// True once the fence behind `index` signals, without waiting.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the fence query fails.
    pub(crate) fn signaled(&self, index: usize) -> Result<bool, VulkanError> {
        self.slots[index].fence.signaled()
    }

    /// Wait `index` out, then free it plus any completed siblings.
    ///
    /// The wait runs outside the queue lock. A timeout or wait failure
    /// resets the ring and fails; the completion is unknown, so no
    /// retry runs.
    ///
    /// # Errors
    ///
    /// Returns `Err` on timeout, on wait failure, or when the reset
    /// after either fails.
    pub(crate) fn poll(&mut self, index: usize) -> Result<(), VulkanError> {
        self.poll_timeout(index, crate::raw::SUBMIT_TIMEOUT_NS)
    }

    /// Wait `index` out up to `timeout_ns`, then free completed slots.
    ///
    /// Same reset contract as [`poll`](Self::poll) under a caller budget.
    ///
    /// # Errors
    ///
    /// Returns `Err` on timeout, on wait failure, or when the reset
    /// after either fails.
    pub(crate) fn poll_timeout(
        &mut self,
        index: usize,
        timeout_ns: u64,
    ) -> Result<(), VulkanError> {
        if self.poisoned {
            self.reset_ring()?;
        }
        match self.slots[index].fence.wait(timeout_ns) {
            Ok(true) => {
                self.slots[index].busy = false;
                self.free_completed()?;
                Ok(())
            }
            Ok(false) => {
                eprintln!("vulkan: fence wait timed out on slot {index}; resetting");
                self.reset_ring()?;
                Err(VulkanError::Timeout)
            }
            Err(error) => {
                eprintln!("vulkan: fence wait failed ({error}); resetting");
                self.reset_ring()?;
                Err(error)
            }
        }
    }

    /// Wait `index` out up to `timeout_ns`, leaving the ring intact on lapse.
    ///
    /// `Ok(false)` reports the lapsed budget; the completion stays
    /// pending for a later wait. A failed wait still resets the ring.
    ///
    /// # Errors
    ///
    /// Returns `Err` on wait failure, or when the reset after it fails.
    pub(crate) fn try_poll_timeout(
        &mut self,
        index: usize,
        timeout_ns: u64,
    ) -> Result<bool, VulkanError> {
        if self.poisoned {
            self.reset_ring()?;
        }
        match self.slots[index].fence.wait(timeout_ns) {
            Ok(true) => {
                self.slots[index].busy = false;
                self.free_completed()?;
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                eprintln!("vulkan: fence wait failed ({error}); resetting");
                self.reset_ring()?;
                Err(error)
            }
        }
    }

    /// Queue-submit `index` once, tracking its fence.
    fn submit_once(&mut self, index: usize) -> Result<(), VulkanError> {
        if self.fail_next > 0 {
            self.fail_next -= 1;
            eprintln!("vulkan: injected submit fault fires on slot {index}");
            return Err(VulkanError::Vulkan(vk::Result::ERROR_UNKNOWN));
        }
        let cmd = self.slots[index].cmd;
        let fence = self.slots[index].fence.clone();
        crate::raw::submit_nowait(
            &self.device,
            &self.queue_lock,
            self.queue,
            cmd,
            fence.fence(),
        )?;
        self.inflight.submit(SlotFence { slot: index, fence });
        self.slots[index].busy = true;
        self.submitted += 1;
        self.peak = self.peak.max(self.inflight.len());
        Ok(())
    }

    /// First free slot from `next`, if any.
    fn free_from_next(&self) -> Option<usize> {
        (0..self.slots.len())
            .map(|step| (self.next + step) % self.slots.len())
            .find(|&index| !self.slots[index].busy)
    }

    /// Claim `index`, resetting its fence and command buffer.
    fn take(&mut self, index: usize) -> Result<usize, VulkanError> {
        self.slots[index].fence.reset()?;
        self.pool.reset_buffer(self.slots[index].cmd)?;
        self.slots[index].busy = true;
        self.next = (index + 1) % self.slots.len();
        Ok(index)
    }

    /// Release `index` without submitting, after a pre-submit failure.
    pub(crate) fn release(&mut self, index: usize) {
        self.slots[index].busy = false;
    }

    /// Free slots whose fences signaled, without blocking.
    fn free_completed(&mut self) -> Result<(), VulkanError> {
        for token in self.inflight.reap_ready()? {
            self.slots[token.slot].busy = false;
        }
        Ok(())
    }

    /// Idle the device, then free every uncompleted slot.
    ///
    /// Completed siblings keep their fences and tokens, so their
    /// readbacks still collect; their command buffers reset at the
    /// next acquire. A failed idle poisons the ring: fences may still
    /// run, so nothing resets until an idle succeeds.
    fn reset_ring(&mut self) -> Result<(), VulkanError> {
        if let Err(error) = crate::raw::wait_idle(&self.device) {
            self.poisoned = true;
            eprintln!("vulkan: idle wait failed ({error}); ring stays poisoned");
            return Err(error);
        }
        self.poisoned = false;
        self.inflight.drain(0)?;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.busy {
                continue;
            }
            if slot.fence.signaled()? {
                let fence = slot.fence.clone();
                self.inflight.submit(SlotFence { slot: index, fence });
            } else {
                slot.fence.reset()?;
                slot.busy = false;
            }
        }
        self.resets += 1;
        eprintln!("vulkan: submit ring reset ({} total)", self.resets);
        Ok(())
    }
}

impl Drop for SubmitSlots {
    fn drop(&mut self) {
        if let Err(error) = crate::raw::wait_idle(&self.device) {
            eprintln!("vulkan: idle wait at teardown failed ({error})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SubmitSlots, SUBMIT_RETRIES, SUBMIT_SLOTS};

    /// Live device plus ring; the ring binding drops first, idling live work.
    fn ring() -> Option<(crate::device::VulkanDevice, SubmitSlots)> {
        let device = crate::device::VulkanDevice::open(crate::ladder::CodecOp::H264Decode)
            .expect("negotiation runs")?;
        let slots = SubmitSlots::open(
            device.vk_device(),
            device.queue_family(),
            device.queue(),
            device.queue_lock().clone(),
        )
        .expect("ring opens");
        Some((device, slots))
    }

    /// Empty recording on `index`.
    fn record_empty(slots: &SubmitSlots, index: usize) {
        let cmd = slots.cmd(index);
        slots.pool().begin_buffer(cmd).expect("begin");
        slots.pool().end_buffer(cmd).expect("end");
    }

    #[test]
    fn stats_start_zeroed() {
        assert_eq!(
            super::SubmitStats::new(),
            super::SubmitStats::snapshot(0, 0, 0, 0)
        );
        assert_eq!(super::SubmitStats::default(), super::SubmitStats::new());
    }

    #[test]
    fn empty_submit_cycle_completes() {
        let Some((_device, mut slots)) = ring() else {
            println!("declined: submit ring needs a video device");
            return;
        };
        assert_eq!(slots.stats().submitted(), 0);
        assert_eq!(slots.take_submitted(), 0);
        assert_eq!(slots.take_creates(), 1 + 2 * SUBMIT_SLOTS as u64);
        let index = slots.acquire().expect("acquire");
        record_empty(&slots, index);
        slots.submit(index).expect("submit");
        slots.poll(index).expect("poll");
        assert_eq!(slots.stats().peak_inflight(), 1);
        assert_eq!(slots.stats().resets(), 0);
        assert_eq!(slots.stats().submitted(), 1);
        assert_eq!(slots.take_submitted(), 1);
        assert_eq!(slots.stats().submitted(), 0);
    }

    #[test]
    fn back_to_back_submits_overlap() {
        let Some((_device, mut slots)) = ring() else {
            println!("declined: submit ring needs a video device");
            return;
        };
        let mut indices = Vec::new();
        for _ in 0..SUBMIT_SLOTS {
            indices.push(slots.acquire().expect("acquire"));
        }
        for &index in &indices {
            record_empty(&slots, index);
        }
        for &index in &indices {
            slots.submit(index).expect("submit");
        }
        assert_eq!(slots.stats().peak_inflight(), SUBMIT_SLOTS);
        for index in indices {
            slots.poll(index).expect("poll");
        }
        assert_eq!(slots.take_resets(), 0);
    }

    #[test]
    fn injected_fault_resets_and_retries() {
        let Some((_device, mut slots)) = ring() else {
            println!("declined: submit ring needs a video device");
            return;
        };
        slots.inject_fault();
        let index = slots.acquire().expect("acquire");
        record_empty(&slots, index);
        slots.submit(index).expect("retry succeeds");
        slots.poll(index).expect("poll");
        assert_eq!(slots.stats().resets(), 1);
        assert_eq!(slots.take_resets(), 1);
        assert_eq!(slots.stats().resets(), 0);
    }

    #[test]
    fn retries_stay_bounded() {
        let Some((_device, mut slots)) = ring() else {
            println!("declined: submit ring needs a video device");
            return;
        };
        for _ in 0..=SUBMIT_RETRIES + 5 {
            slots.inject_fault();
        }
        let index = slots.acquire().expect("acquire");
        record_empty(&slots, index);
        assert!(slots.submit(index).is_err());
        assert_eq!(slots.stats().resets(), 1);
    }
}
