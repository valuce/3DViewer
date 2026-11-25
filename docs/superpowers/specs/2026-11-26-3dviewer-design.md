# 3DViewer — design

Date: 2026-11-26. Status: approved by user ("Да, погнали").

## Intent

A very fast, lightweight 3D model viewer for Windows, written in Rust.
Open a file (double click / drag-and-drop / command line) and see it immediately.
Covers most common formats, has several display modes and a model-info panel.

## Goals

- Single portable `.exe`, no runtime dependencies, small (target ~10–15 MB).
- Cold start to first frame well under a second; model loading on a background thread.
- Idle CPU ~0 (reactive repaint, redraw only on input/changes).
- Formats: glTF/GLB, OBJ+MTL, STL (ascii/binary), PLY (ascii/binary), FBX, 3MF, OFF.
- Display modes: shaded, shaded+wireframe, wireframe, points, clay, unlit/texture,
  normals, UV checker, vertex colors. Toggles: textures, flat shading, backface culling,
  grid, axes, bounding box, background.
- Camera: orbit / pan / zoom, frame (F), front/side/top views, perspective/ortho, Y-up/Z-up.
- Info: file, format, size, load time, vertices/triangles, meshes, materials, textures
  (resolution), bounds/dimensions, nodes, animations, skins/bones, warnings.
- Browse neighbouring files in the same folder (PgUp/PgDn), recent files list.

## Non-goals (for now)

- Editing, export, animation playback, IBL / HDR environments.
- Draco / meshopt compressed glTF, KTX2/Basis textures.

## Anti-goals

- Heavy engine (Bevy) or Electron-like runtime; long startup; constant GPU/CPU load when idle.

## Approaches considered

1. **wgpu + egui (eframe), native loaders + ufbx** — chosen. Small exe, fast start,
   modern backends (DX12/Vulkan), fast purpose-built parsers.
2. Assimp (russimp) for everything — 40+ formats but +~15 MB, slower loaders,
   C++ build. Kept as a possible optional cargo feature for exotic formats (DAE, 3DS, BLEND, X).
3. Bevy / three-d — heavy, slow compile, big binary, overkill for a viewer.

## Design

- `eframe` (wgpu backend only) owns window, device, egui.
- 3D scene rendered to an offscreen MSAA texture (own depth), resolved and shown as an
  egui image in the central panel; rendering encoded in `update()` on the shared queue.
- Loader layer produces a format-neutral `Scene` (meshes with positions/normals/uvs/colors,
  u32 indices, topology, material index; materials; decoded RGBA8 textures; info).
  Node transforms baked into vertices. Missing normals generated.
- Loaders: `gltf` crate (buffers) + parallel image decode; `ufbx` for FBX and OBJ;
  own fast parsers for STL, PLY, OFF; 3MF via `zip` + `quick-xml`.
- Shading: simplified PBR (GGX key light attached to camera + hemisphere ambient),
  normal maps via screen-space derivatives (no tangents needed), flat shading via derivatives.
- Wireframe via `PolygonMode::Line` (fallback: overlay disabled if unsupported);
  points as instanced screen-space quads.
- GPU mipmap generation for textures.
- Settings & recent files persisted by eframe.

## Left to the builder

Exact UI layout, shortcuts, colors, crate versions.
