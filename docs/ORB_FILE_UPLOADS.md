# Orb file attachments

The composer's **+ → Upload file or image…** works even when the project has no
context files. Desktop uses a native multi-file picker; the browser uses a file
input. A local agent receives the selected absolute path without a server copy.
Core and remote-node agents receive a path returned by the destination's upload
receipt. Files are uploaded before their paths enter the draft.

A machine/backend change is checked again on Send. References still in the
draft are transferred to the new destination and rewritten before launch or
follow-up. Removing a reference prevents another transfer. Failed transfers
preserve the draft and show an error. Selecting files never launches a mission.

`POST /api/uploads` is dashboard-authenticated and accepts `{node_id,name,
data_base64}`. Core stores locally for `core`, or forwards to the configured
node's bearer-authenticated `POST /uploads`. There is no host path supplied by
the client. Receipts contain absolute path, byte count and SHA-256. These paths
are ordinary prompt references, not project-snapshot attachments, so uploads
also work for remote launches and follow-ups without the legacy attachment
materializer.

Remote uploads are limited to 20 MiB per file and four concurrent store/forward
operations per process. Native local references have no upload size limit.
Bytes are stored under unique UUID directories, never overwriting a file of the
same name. A failed write removes its partial file. Storage is
`<resolved .sandboxed-sh>/uploads` on Core and `<node work root>/uploads` on nodes.
Uploaded files persist for later turns; removing their text reference does not
delete the server copy. They must not be swept as orphaned mission directories.
An eventual project/file retention UI should expose deletion explicitly.

Node binaries need the upload route; old nodes refuse clearly rather than
silently placing files on Core. No file-server credential reaches Orb or a
mission. Desktop binary reads are limited to files explicitly selected in that
process's native picker. Historical project mentions (`@file`, `@controller`)
retain their existing snapshot behavior.

Validation: byte-exact binary persistence and traversal rejection unit tests;
an actual temporary node rejects unauthenticated uploads and accepts binary
uploads; composer tests cover an empty context menu and failure preservation;
Playwright covers the file chooser, byte encoding, and Core → Ashur switching
before Send. The desktop Rust build and frontend build pass.

Draft persistence keeps uploaded paths and their destination. After restarting
Orb, an already-uploaded file can be reused on the same server/machine. Moving
a restored draft to another machine may require selecting the original file
again: browser file bytes and native-picker read authorization are not retained
across restarts. A transfer failure leaves the draft intact.

Production verification on 22 September 2026: Core deployed commit `55867c859`
through the guarded deploy endpoint, followed by a healthy Hermes restart.
The desktop debug build was relaunched with the native picker commands. The
merged UI passed 325 unit/component tests and both browser tests (file chooser
with machine switching, and image paste). Live binary uploads were forwarded
through Core and read back byte-for-byte on Core, Ashur, Nippur, old-agent,
sepolia and DGX Spark; remote reads used the `sandboxed-node` account. Only the
diagnostic upload files were removed afterward.

Babylon is the exception: its updated harnesses are installed, but the node
upload-route binary rollout is blocked by network packet loss and incomplete
artifact transfers. Its prior node executable was kept intact. Do not send file
missions there until connectivity, the node update and byte-exact readback have
all passed. The other nodes retain a backup of the replaced node executable.
