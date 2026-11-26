# 3DViewer

<img src="assets/icon_256.png" width="96" align="right" alt="">

Fast, lightweight 3D model viewer for Windows, written in Rust.
One ~18 MB exe with no dependencies: install it with the setup or just run it.
The window opens instantly, models load in the background, and the CPU stays idle when nothing changes.
The interface is in English and Russian (follows the system language).

## Download

The [Releases](../../releases) page has three options:

- `3DViewer-X.Y.Z-setup.exe` — installer. Installs for the current user into
  `%LOCALAPPDATA%\Programs\3DViewer` without admin rights, adds Start menu and (optionally) desktop
  shortcuts and offers per-format file associations. Uninstall from Settings → Apps.
- `viewer3d.exe` — the same program without installation; just run it.
- `3DViewer-X.Y.Z-windows-x64.zip` — the exe with this README and the license.

After that the app updates itself: Help → Check for updates….

## Formats

| Format | What is read |
|---|---|
| glTF / GLB | meshes, node hierarchy, PBR materials, textures, vertex colours, **animations, skins, morph targets**; **Draco** and **meshopt** compression, quantization (KHR_mesh_quantization), KHR_texture_transform |
| FBX | via [ufbx](https://github.com/ufbx/ufbx): meshes, materials, embedded and external textures, **animations, skins, blend shapes** |
| OBJ + MTL | via ufbx |
| COLLADA (DAE) | all primitive types, hierarchy, skin/morph controllers, profile_COMMON materials and textures |
| 3DS | objects, materials, textures, smoothing groups |
| BLEND | Blender 2.8–5.x files (compressed too): meshes, UVs, materials, object transforms; modifiers are not applied |
| STL | binary and ASCII, VisCAM/SolidView colours |
| PLY | ASCII and binary, meshes and point clouds, Gaussian splats (colour from SH DC) |
| 3MF | core, base materials, colour groups, components, production extension (Bambu Studio, Prusa, Orca) |
| OFF | OFF / COFF / NOFF |

## Features

- **Display modes:** PBR materials, clay, unlit, normals, UV checker, vertex colours, hidden-line wireframe, points.
- **Toggles:** edges over the model, textures, flat shading, vertex colours, back-face culling, grid, axes,
  bounding box, orthographic projection, Z-up.
- **Animation:** play/pause, one clip or all clips at once, scrubbing, speed, looping.
- **View gizmo** (bottom left, like Unity's): click an axis to look along it; the cube in the middle and the
  caption below toggle perspective / orthographic.
- **Walk:** WASD or arrows, Q/E down/up, Shift to run, drag the mouse to look, wheel for speed — for large
  locations.
- **Ruler and angle:** click points right on the model surface (animation and the section are respected).
  Distances are shown in mm / cm / m using the file's unit (glTF, FBX, 3MF, DAE, BLEND); for OBJ, STL and
  others you pick the unit. The angle is measured through three points, the vertex in the middle.
- **Section:** a cutting plane along X, Y or Z with a 0…1 slider and a flip; the inside faces are shown in red.
- **Environment (HDRI):** lighting and reflections from a panorama — studio, sky, sunset, overcast or your own
  `.hdr`; intensity, rotation, and an optional (blurred) background.
- **Pivot:** the grid goes through the file origin (the model's pivot) with coloured axis lines, and a yellow
  ring marks the pivot even through the model. The info panel tells where the pivot sits in the bounds
  ("bottom centre", "centre" or per-axis percentages). When the pivot is far from the model the grid is put
  under the model instead.
- **Background:** presets, a solid colour or a two-colour gradient.
- **Model info:** format, parse and GPU upload time, GPU memory, vertices and triangles, mesh list with
  visibility and highlighting, materials, textures with resolution, bounds, nodes, animations, bones,
  morph targets, loader warnings; the summary can be copied.
- **File associations:** File → File associations… shows every format with a checkbox and the app that opens
  it now. Ticked formats get 3DViewer in "Open with" (no admin rights), unclaimed ones also open on
  double-click. Where another app is already the default, "Make default…" opens 3DViewer's page in Windows
  settings. From the command line: `viewer3d.exe --register` (all formats), `--register glb stl` (only these),
  `--unregister` (remove everything).
- **Updates:** once a day on start-up the latest GitHub release is checked (can be turned off). A new version
  is downloaded, verified against the SHA-256 digest GitHub publishes, test-started and swapped in; the old
  file is removed on the next start.
- Esc leaves the active tool, then turns the section off.

## Controls

| Key | Action |
|---|---|
| LMB | orbit |
| RMB / MMB / Shift+LMB | pan |
| Wheel | zoom |
| Double click, F | frame the model |
| R | reset the view |
| 1 … 8 | display modes |
| W / T / L / V / C | edges / textures / flat / vertex colours / culling |
| G / X / B | grid / axes / bounds |
| P / U | orthographic / Z-up |
| Ctrl+1 / 3 / 7 / 9 | front / right / top / bottom |
| Space | play / pause animation |
| PgUp / PgDn, ← / → | previous / next file in the folder |
| Ctrl+O, F5, F11, F1 | open, reload, full screen, help |
| Gizmo click | look along an axis; cube — perspective / ortho |
| WASD, Q/E, Shift | walk (toolbar tool) |
| LMB / RMB | ruler and angle: place a point / remove the last one |
| Esc | leave the tool, then turn the section off |

Open a file by dropping it on the window or from the command line:

```
viewer3d.exe model.glb
viewer3d.exe --info a.fbx b.stl     # print a summary without opening a window
viewer3d.exe --version
```

## Building

You need Rust (stable, MSVC) and Visual Studio Build Tools (the C/C++ compiler for ufbx and meshoptimizer).

```
cargo build --release
cargo test
cargo test -- --ignored     # live checks of the GitHub API and release download
```

The exe is `target\release\viewer3d.exe`. The C runtime is linked statically (`.cargo/config.toml`),
so the Visual C++ Redistributable is not needed.

The installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php):

```
iscc installer\3DViewer.iss /DAppVersion=0.4.0
```

Output: `target\installer\3DViewer-0.4.0-setup.exe`.

## Layout

- `src/loaders/*` — format loaders; all produce a common `Scene` (`src/scene.rs`).
- `src/anim.rs`, `src/player.rs` — nodes, clips, CPU skinning and morphs, playback.
- `src/render/*` — wgpu renderer (DX12/Vulkan): MSAA 4×, PBR with Khronos Neutral tone mapping, normal maps
  without tangents, GPU mipmaps, wireframe via `PolygonMode::Line`, instanced points, GPU picking;
  `src/render/env.rs` — environment maps (HDRI).
- `src/app.rs`, `src/ui.rs`, `src/i18n.rs` — window, camera, egui/eframe panels, localisation.
- `src/tools.rs` — walk, ruler, angle, section.
- `src/assoc.rs` — file associations in the current user's registry.
- `src/update.rs`, `src/net.rs` — update check and install; HTTPS through the system WinHTTP.
- `installer/3DViewer.iss` — installer (per user, no admin rights).

## License

[MIT + Commons Clause](LICENSE): free to use anywhere, including at work and in commercial projects, and the
code may be studied, modified and shared as long as the license text is kept. Selling the program, modified
versions of it, or paid services whose value comes mainly from it is not allowed. Third-party libraries keep
their own licenses.
