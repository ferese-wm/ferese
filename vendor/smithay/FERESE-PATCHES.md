# Ferese's Smithay dependency

This directory vendors the Smithay library package from
[`8a270232f1bfc416c781273c98a3f52a812a0bba`](https://github.com/Smithay/smithay/tree/8a270232f1bfc416c781273c98a3f52a812a0bba).
The upstream MIT license is preserved in `LICENSE.txt`.

The library sources, examples, benchmark sources, build script and GBM probes are
retained. Upstream's separate example workspaces are omitted and the nested
workspace has no members. Dependency versions and feature definitions are retained.

Local changes are limited to the following library boundaries:

- DRM frame submission can carry an optional asynchronous presentation request
  through Smithay's existing queued/pending/current frame ownership. Ordinary
  `queue_frame` callers retain synchronized presentation. Modesets remain
  synchronized, and rejected asynchronous requests retry ordinary submission.
- Renderer surface attachments consume each explicit synchronization acquire and
  release point, including when a client reattaches the same `wl_buffer`.
- GLES exposes a shared completion snapshot covering normal, dropped, and cleanup-
  error frame lifetimes. Ferese can retain sampled attachments without blocking.
- Syncobj validation rejects attachments with both required points missing; the
  timeline API can identify a point without signaling or submitting it.
- DRM lease-device disconnect removes its known binding, preventing stale device
  objects from accumulating after clients vanish.
- GLES exposes the most recent completed submission handle so the compositor
  can retain sampled explicit-sync buffers across display, capture, offscreen,
  error-cleanup and dropped-frame submission paths.

The compositor controls capability checks and user/client policy. Vendoring this
API extension does not enable tearing or variable refresh by default and does not
establish hardware validation.
