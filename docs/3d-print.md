# 3D printing: Cura integration

OpenCADStudio and [UltiMaker Cura](https://github.com/Ultimaker/Cura) are
two different programs in two different languages (Rust + iced here; Python +
PyQt on top of the Uranium framework and the C++ CuraEngine there), so their
sources cannot be merged into one code base. What *is* one workflow is the
hand-over between modelling and printing, and that is built in:

| Step | In OpenCADStudio |
|---|---|
| Bring a printable model in | **Model › 3D Print › Import Mesh** (`MESHIMPORT`): STL, 3MF or OBJ, scaled to the drawing's units |
| Model / edit | the usual solid tools (Box, Extrude, Boolean, Fillet…) |
| Hand the model to Cura | **Send to Cura** (`CURA`): writes a 3MF in millimetres to a temporary folder and opens it in the installed Cura |
| Slice without leaving the CAD | **Slice G-code** (`GCODE`): runs Cura's own slicer (`CuraEngine`, part of every Cura installation) on the drawing's meshes and saves the G-code |
| Export for any slicer | **Export STL** (`STLOUT`) or **Export 3MF** (`3MFOUT`), both in millimetres |

Cura keeps doing what it is best at (printer profiles, material library,
layer preview, printer connection); the model never has to be exported by
hand again, and the quick "is it printable / how long will it take" loop
runs here.

## Commands

| Command | Aliases | What it does |
|---|---|---|
| `MESHIMPORT` | `IMPORTMESH`, `STLIMPORT`, `IMPORTSTL`, `3MFIMPORT`, `IMPORT3MF` | Pick an STL / 3MF / OBJ file and add its meshes to the drawing (3MF: every build item, components and transforms applied; units converted). |
| `STLOUT` | `EXPORTSTL` | Save every mesh of the drawing as one binary STL, converted from the drawing's units to millimetres (what every slicer assumes). |
| `3MFOUT` | `EXPORT3MF` | Save every mesh of the drawing as one 3MF (`unit="millimeter"`, one object per mesh). |
| `CURA` | `SENDTOCURA`, `PRINT3D` | Export a 3MF to `%TEMP%/OpenCADStudio-print/` and launch Cura with it. Asks for the Cura executable the first time it cannot find one. |
| `CURAPATH` | | Pick the Cura executable; CuraEngine and the printer definitions are looked up next to it and remembered. |
| `GCODE` | `SLICE3D`, `CURASLICE` | Slice the meshes with CuraEngine and save the G-code (`SLICE` stays the CAD command that cuts solids). The command line reports print time, filament and layer count from the G-code header. |
| `GCODESETTINGS` | | Show the slicing profile and where Cura / CuraEngine / definitions were found. |
| `GCODESET name value` | | Change one profile value, e.g. `GCODESET layer_height 0.28`, `GCODESET infill 35`, `GCODESET support on`, `GCODESET adhesion brim`. |

Slicing profile (`GCODESET` names): `layer_height`, `infill` (%), `walls`,
`top_bottom_layers`, `nozzle_temp`, `bed_temp`, `speed` (mm/s), `support`
(on/off), `adhesion` (none/skirt/brim/raft), `bed_width`, `bed_depth`,
`bed_height`, `nozzle`, `filament_diameter`. They are passed to CuraEngine
as `-s key=value` overrides on top of Cura's generic `fdmprinter.def.json`;
everything else keeps Cura's defaults. The model is centred on the bed
(`center_object=true`).

All of these are ordinary commands, so the AI assistant and MCP / REST
clients can run them too (`run GCODESET infill 30`, `run GCODE` …).

## Where Cura is looked for

- Windows: `Program Files\UltiMaker Cura <version>\UltiMaker-Cura.exe` (and
  older `Ultimaker Cura`, `Cura.exe` names), also under `%LOCALAPPDATA%`;
  `CuraEngine.exe` and `share\cura\resources\definitions` next to it.
- macOS: `/Applications/UltiMaker Cura.app` (`Contents/MacOS/CuraEngine`,
  `Contents/Resources/share/cura/resources/definitions`).
- Linux: `cura` / `ultimaker-cura` / `CuraEngine` on `PATH`, Cura AppImages in
  `~/Applications`, `~/Downloads` or `~`, definitions in
  `/usr/share/cura/resources/definitions`.

`CURAPATH` overrides detection and stores the three locations in
`settings.json` under `"print3d"` (`cura_path`, `engine_path`,
`definitions_dir`), where they can also be edited by hand — useful for an
AppImage, which has no CuraEngine outside the image.

## Units

Drawings carry `INSUNITS`; meshes are scaled to millimetres on export and
from the file's unit (3MF `unit`, STL/OBJ assumed millimetres) into the
drawing's unit on import. A unitless drawing counts as millimetres.

## Files

- `src/io/stl.rs` — binary/ASCII STL reader next to the existing writer.
- `src/io/threemf.rs` — 3MF writer and reader (core spec: meshes, components,
  build transforms, units).
- `src/app/print3d.rs` — commands, Cura/CuraEngine detection, slicing, the
  slicing profile and its persistence.
- `src/modules/model/mod.rs` — the **3D Print** ribbon group.

## Not done / next steps

- A G-code layer preview inside OpenCADStudio (Cura's preview remains the
  place for that; `CURA` opens the model there).
- Printer-specific profiles: `GCODE` uses the generic FDM printer definition
  with the sizes from `GCODESET`; to print with a specific machine profile
  and material, hand the model to Cura.
- Opening `.stl` / `.3mf` directly from File › Open (use `MESHIMPORT` into a
  new drawing for now).
