# Stage 04-E verification: camera, visual search and translation

Development-only fixtures and acceptance results.

## Synthetic camera harness

There is no physical camera on the development machine. Camera tests use
synthetic PipeWire cameras published by GStreamer, exactly as a camera node
appears to the XDG Camera portal:

- `camera_source.sh` publishes a known image (or the test pattern) through
  `pipewiresink mode=provide` with `media.class=Video/Source` and
  `media.role=Camera`, in YUY2, BGRx or I420.
- `make_fixtures.py` draws the known images: text ("CAMERA TEST 42",
  "SECOND CAMERA", "Bonjour le monde") and simple objects (a red square, a
  blue circle, a green triangle) at known positions.
- `camera_probe.py` checks the portal's presence, access response and
  descriptor handoff on the private session bus.
- `focus_window.py` maps a window carrying the application ID, because
  GNOME Shell shows the camera Access dialog only for the focused app.
- `run_camera_test.sh <test exe> <output dir>` runs
  `camera::tests::portal_camera_snapshots_in_a_headless_session` inside the
  03-E disposable session (`docs/feasibility-03-e/probe/session.sh` and
  `with-portal.sh`) with private user, network and PID namespaces. The PID
  namespace releases the private D-Bus, PipeWire, WirePlumber, GNOME Shell
  and portal processes on exit. The test sets permissions with `gdbus`
  inside the disposable profile.

The test drives the production code: `camera::open` (Camera portal
`IsCameraPresent`, `AccessCamera`, `OpenPipeWireRemote`) and
`capture::pipewire::capture` (registry listing, camera choice, YUY2/RGB
negotiation, one settled frame, teardown).

Build the test binary with
`cargo test --release --locked -p local-first-ai-agent --no-run` and pass the
`Executable unittests` path.

## Packaged WebView fixture

`desktop_acceptance::visual_desktop_fixture` runs the packaged main window
and contextual panel (`npm run build`, then
`cargo test --release -p local-first-ai-agent --features tauri/custom-protocol`)
under Xvfb in `unshare -r -n` with a disposable encrypted profile. The host's
test-only camera fixture supplies synthetic frames in place of the portal
(the portal path is covered by the headless-session test above). Screenshots
are written when `AGENT_GLANCE_SCREENSHOTS` names a directory.

## Results

The production camera path passed with synthetic PipeWire cameras. It
recognized the test frames in YUY2 and BGRx, offered both cameras before
opening the selected stream, and handled absent cameras, permission denial,
consent cancellation, disconnect and unsupported I420 input. Each of five
repeated snapshots ended with eight descriptors and six threads.
Physical-camera compatibility is unverified.

The combined packaged WebView fixture passed in the disposable encrypted
profile. Chat previewed and cropped a camera snapshot, attached it with
camera provenance, extracted English text, populated an editable search
query, and translated recognized text. The contextual panel previewed and
confirmed its snapshot, ran local OCR and translation, and explained why
search was unavailable in Local-only mode. Its translation receipt showed
camera text and no transmission. The profile scan found zero image files
and no Pictures directory.

Loopback research tests check the exact query received by the provider.
Edited OCR queries and reviewed model results retain their source classes
in the egress receipt. An attachment-derived search requires an explicit
query. The policy excludes ordinary document attachments, document excerpts,
memory and history, including those inherited by a model's result.

The existing ScreenCast portal regression passed through consent,
in-memory capture, crop and OCR. Its back-to-back test passed three cycles
and ended with the same 11 descriptors and seven threads as after the first
cycle. The first attempt stalled at consent. The retry used the overlay
driver's absolute-pointer click in the disposable session.

Release Clippy with warnings denied, the locked packaged desktop test build,
rustfmt and the focused research, frame conversion and EXIF tests passed.
The previous storage, desktop unit and frontend test results were reused.
Search verification used loopback providers.

## JPEG dependencies

- `zune-jpeg 0.4.21` decodes oriented JPEGs. `zune-core 0.4.12` supplies
  the decoder configuration. Both declare MIT OR Apache-2.0 OR Zlib.
- `jpeg-encoder 0.7.1` writes the upright JPEG and declares
  (MIT OR Apache-2.0) AND IJG. A redistributed executable needs the selected
  permissive license notices and the IJG acknowledgement: “this software
  is based in part on the work of the Independent JPEG Group”. If
  distributing the IJG-derived source, include its copyright and warranty
  notice and identify modifications as required by the IJG terms.
