# Aluminium profile design

The **Aluminium** ribbon tab brings the MayCAD-style workflow for T-slot
extrusions into OpenCADStudio: pick a profile from the catalogue, place it by
length and direction (or build a whole box frame in one go), let the
software put angle brackets on every joint, and read off the parts list and
an optimised cut list — all as real 3D solids that the rest of the CAD
(sections, booleans, STEP/STL/3MF export, the AI assistant, MCP) can use.

## Workflow

1. **Profile** (dropdown): choose a size such as `40x40` or `40x80`, type the
   length in mm (or press Enter to set it by the end point), click the start
   point and the end point. The profile is extruded along that direction with
   its height vertical.
2. **Frame**: profile, length × width × height in mm and a corner point give
   twelve members — four along X, four fitted between them along Y and four
   verticals — plus an angle bracket at each of the joints.
3. **Connect**: with profiles selected (or none selected = the whole
   drawing), every profile end that rests on another profile's face gets an
   angle bracket of the matching series. Running it again never doubles a
   bracket.
4. **Length**: select profiles, then `ALULENGTH 850` rebuilds them at
   850 mm keeping start point and direction.
5. **Parts List**: inserts a table (position, part number, description,
   length, quantity, weight) and prints it; **Cut List** packs the pieces into
   stock bars (default 6000 mm, saw kerf 3 mm) with the waste per bar;
   **CSV** writes both to a file for purchasing or the saw.

## Commands

| Command | Arguments | Notes |
|---|---|---|
| `ALUPROFILE` (`ALU`) | `[profile] [length mm] [x,y,z] [X\|Y\|Z\|dx,dy,dz]` | Anything missing is asked for: profile buttons, length, two points. `ALUPROFILE 40x80 600 0,0,0 Z` is fully scripted. |
| `ALUFRAME` | `[profile] [L W H] [x,y,z]` | Box frame with brackets. |
| `ALUCONNECT` | `[ALL]` | Brackets for the selected profiles; `ALL` or no selection = every profile. |
| `ALULENGTH` | `<mm>` | New length for the selected profiles. |
| `ALUBOM` | `[x,y]` | Parts-list table at the point (default origin) + command-line listing. |
| `ALUCUTLIST` | `[stock mm] [kerf mm]` | First-fit-decreasing cut plan per part number. |
| `ALUBOMCSV` | `[path]` | Parts list and cut plan as CSV (file dialog without a path). |
| `ALUCATALOG` | | Lists the catalogue with slot, bore and weight. |

All are ordinary commands: the AI assistant can run them (`run ALUFRAME 40x40
800 600 700 0,0,0`) and so can MCP / REST clients.

## Catalogue

A generic T-slot catalogue with three slot series (6 / 8 / 10 mm) and typical
weights; part numbers follow `AP-<w>-<h>`, angle brackets `AB-<module>`.

| Profile | Slot | Core bore | kg/m | Profile | Slot | Core bore | kg/m |
|---|---|---|---|---|---|---|---|
| 20x20, 20x40, 20x60, 20x80 | 6 | 4.3 | 0.42 – 1.40 | 45x45, 45x90 | 10 | 8.5 | 1.90 – 3.60 |
| 30x30, 30x60 | 8 | 6.8 | 0.90 – 1.60 | 50x50, 50x100 | 10 | 8.5 | 2.30 – 4.40 |
| 40x40, 40x80, 40x120, 40x160 | 8 | 6.8 | 1.50 – 5.70 | 60x60, 80x80, 90x90 | 8 / 8 / 10 | 6.8 / 6.8 / 8.5 | 2.80 – 6.00 |

The cross-section is generated parametrically (`src/app/aluprofile.rs`,
`outline`): one T-slot per base module on every face (opening = slot width,
lip, undercut cavity), small corner chamfers, and the core bore subtracted
from the extrusion. To add a supplier's own series, add a `spec!` line to
`CATALOG` (dimensions, slot, module, bore, weight) and a dropdown entry in
`src/modules/alu/mod.rs`; everything else follows from the catalogue.

## How members are stored

Every member is a `3DSOLID` carrying extended data under the application
`OCS_ALU`: kind (`profile` / `connector`), part number, name, length in mm,
start point and direction. The parts list, cut list, `ALULENGTH` and the joint
finder read that tag, so members survive save/open, copy and move (a moved
copy keeps its tag; run `ALUCONNECT` again for its joints). Deleting the tag
(or a solid made another way) simply leaves the solid out of the lists.

Lengths are always millimetres; drawings in other units (`INSUNITS`) are
converted on placement.

## Joints

`ALUCONNECT` looks for a profile end whose axis meets the face of another,
roughly perpendicular profile (within 0.5 mm). The bracket is an L of two
plates (module × module, thickness 10 % of the module) in the plane of the two
members, on the side where the second member continues; it is added as a
connector member so it appears in the parts list. Brackets are visual and
countable — slot nuts and screws are not modelled.

## Limits / next steps

- One bracket per joint (on the continuing side); mirrored pairs, gussets,
  internal connectors and end caps can be added as further connector kinds.
- The cross-section is a close generic approximation, not a supplier's exact
  drawing; mass comes from the catalogue's kg/m, not the solid's volume.
- No 2D shop drawing is generated automatically; use layouts and viewports
  on the model, plus the parts-list table.
