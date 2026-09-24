# Orb artwork

`Orb.icon/Assets/orb.png` is the supplied artwork. The centered layer is scaled
to 80% to leave more black space around the motif. `Orb.icon` is an Apple Icon Composer
document. Open it in Icon Composer to tune the native material and lighting.

On macOS, Tauri's platform config runs `scripts/build-macos-icon.mjs` before
building. This requires Xcode 26 or newer and compiles the document with
`actool`. The bundle includes `Assets.car` (native icon stacks and appearance
variants) and `Orb.icns` (legacy fallback); `Info.plist` selects `Orb`.
Do not replace this with only a PNG-to-ICNS conversion: that loses the system
material rendering. Other platforms use the PNG sizes in this directory.

The Dock's native icon is used by the packaged `.app`. A `tauri dev` process
does not have the packaged asset catalog. Navbar artwork hot-reloads normally.
