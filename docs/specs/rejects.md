<!-- source: codec mapping sweep, 2026-10-11; tool: hand -->
<!-- date: 2026-10-11; author: rust-video -->
# Rejects

Claims checked and set aside. The spec files state what holds; this
file records what did not, so no pass re-finds it.

- H.264 tiles: none exist. Rejected as a caps dimension.
- VP9 draft-grange-00: overview snapshot with no profiles, levels, or
  caps. Rejected as a normative source; v0.7 plus the levels page won.
- AV1 level indexes 2, 3, 6, 7, 10, 11, 20-23: undefined by the level
  formula. Rejected as valid levels.
- Secondary AV2 sources (decoder-crate readouts): unneeded. The v1.0.0
  specification fetched directly and won as primary.
- Fixed hardware resolution caps: none published by any of the four
  tier APIs. Rejected as constants; runtime queries won.
- VP9 CPB rows at 5.2/6.x: the levels page marks them TBD. Unmapped,
  not zero.
- AV2 level-31 maximum row: exists, values uninspected. Unmapped.
- H.264 SVC/MVC profile ids: counted, not enumerated. Unmapped.
- DXVA VP9/AV1 GUID hex: closed by rule, windows-crate constants win.
- CDD certification floors, AV1/AV2 Annex E timing semantics: unmapped.
