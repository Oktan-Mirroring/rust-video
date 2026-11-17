//! Off-target OS state: portable handles carry no platform resources.

/// Device OS state off-target: no resources.
///
/// The device type keeps its shape on every target; only Windows
/// constructs it.
pub(crate) type DeviceOs = ();
