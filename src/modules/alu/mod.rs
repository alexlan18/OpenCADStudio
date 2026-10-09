// Aluminium module — T-slot profile design: catalogue, members, frames,
// connectors, parts lists and cut lists (see `app::aluprofile`).

use crate::modules::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};

const PROFILE_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/profile.svg");
const FRAME_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/frame.svg");
const CATALOG_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/catalog.svg");
const CONNECT_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/connect.svg");
const LENGTH_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/length.svg");
const BOM_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/bom.svg");
const CUTLIST_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/cutlist.svg");
const CSV_ICON: &[u8] = include_bytes!("../../../assets/icons/alu/csv.svg");

fn tool(id: &'static str, label: &'static str, icon: &'static [u8]) -> ToolDef {
    ToolDef {
        id,
        label,
        icon: IconKind::Svg(icon),
        event: ModuleEvent::Command(id.to_string()),
    }
}

/// The profile dropdown: one entry per catalogue size, each starting
/// `ALUPROFILE <size>` so only length and points remain to be given.
const PROFILE_SIZES: &[(&str, &str)] = &[
    ("ALUPROFILE 20x20", "20x20"),
    ("ALUPROFILE 20x40", "20x40"),
    ("ALUPROFILE 20x60", "20x60"),
    ("ALUPROFILE 20x80", "20x80"),
    ("ALUPROFILE 30x30", "30x30"),
    ("ALUPROFILE 30x60", "30x60"),
    ("ALUPROFILE 40x40", "40x40"),
    ("ALUPROFILE 40x80", "40x80"),
    ("ALUPROFILE 40x120", "40x120"),
    ("ALUPROFILE 40x160", "40x160"),
    ("ALUPROFILE 45x45", "45x45"),
    ("ALUPROFILE 45x90", "45x90"),
    ("ALUPROFILE 50x50", "50x50"),
    ("ALUPROFILE 50x100", "50x100"),
    ("ALUPROFILE 60x60", "60x60"),
    ("ALUPROFILE 80x80", "80x80"),
    ("ALUPROFILE 90x90", "90x90"),
];

pub struct AluModule;

impl CadModule for AluModule {
    fn id(&self) -> &'static str {
        "alu"
    }
    fn title(&self) -> &'static str {
        "Aluminium"
    }

    fn ribbon_groups(&self) -> &[RibbonGroup] {
        static GROUPS: std::sync::OnceLock<Vec<RibbonGroup>> = std::sync::OnceLock::new();
        GROUPS.get_or_init(|| {
            vec![
                RibbonGroup {
                    title: "Profiles",
                    tools: vec![
                        RibbonItem::LargeDropdown {
                            id: "ALU_PROFILES",
                            label: "Profile",
                            icon: IconKind::Svg(PROFILE_ICON),
                            items: PROFILE_SIZES
                                .iter()
                                .map(|(command, label)| (*command, *label, IconKind::Svg(PROFILE_ICON)))
                                .collect(),
                            default: "ALUPROFILE 40x40",
                        },
                        RibbonItem::LargeTool(tool("ALUFRAME", "Frame", FRAME_ICON)),
                        RibbonItem::Tool(tool("ALUCATALOG", "Catalog", CATALOG_ICON)),
                    ],
                },
                RibbonGroup {
                    title: "Joints",
                    tools: vec![
                        RibbonItem::LargeTool(tool("ALUCONNECT", "Connect", CONNECT_ICON)),
                        RibbonItem::Tool(tool("ALULENGTH", "Length", LENGTH_ICON)),
                    ],
                },
                RibbonGroup {
                    title: "Lists",
                    tools: vec![
                        RibbonItem::LargeTool(tool("ALUBOM", "Parts\nList", BOM_ICON)),
                        RibbonItem::Tool(tool("ALUCUTLIST", "Cut List", CUTLIST_ICON)),
                        RibbonItem::Tool(tool("ALUBOMCSV", "CSV", CSV_ICON)),
                    ],
                },
            ]
        })
    }
}
