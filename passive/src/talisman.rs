use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH, Instant};

use eldenring::cs::{ChrInsExt, ItemCategory, PlayerIns};
use eldenring::fd4::FD4ParamRepository;
use eldenring::param::EQUIP_PARAM_ACCESSORY_ST;
use eldenring::param::EquipParam;
use eldenring::cs::InventoryItemsData;
use fromsoftware_shared::FromStatic;
use once_cell::sync::Lazy;
use std::fs::OpenOptions;
use std::io::Write;
use std::env;
use std::path::PathBuf;

/// Track last applied special effects per-player.
struct TalismanState {
    player_addr: usize,
    last_sps: HashSet<i32>,
    // Optional deadline to run the post-apply maxing step.
    wait_until: Option<Instant>,
}

static TALISMAN_STATE: Lazy<Mutex<Option<TalismanState>>> = Lazy::new(|| Mutex::new(None));

static DEBUG: bool = false;

/// Extract all valid SP effects from a single accessory
fn accessory_speffects(acc: &EQUIP_PARAM_ACCESSORY_ST) -> impl Iterator<Item = i32> + '_ {
    [acc.ref_id()].into_iter().filter(|&id| id > 0)
}

/// Collect all unique accessory SP effect ids from the player's inventory.
///
/// Fix: only consider items whose category is Accessory. This prevents goods (and other categories)
/// that share numeric param IDs from being misinterpreted as accessory param rows.
fn collect_inventory_accessory_speffects(player: &PlayerIns) -> HashSet<i32> {
    let mut out = HashSet::new();

    let repo = unsafe { FD4ParamRepository::instance().unwrap() };

    let pg = unsafe { player.player_game_data.as_ref() };
    let inv = &pg.equipment.equip_inventory_data.items_data;

    for entry in InventoryItemsData::items(inv) {
        if entry.item_id.category() != ItemCategory::Accessory {
            continue;
        }

        let param_id = entry.item_id.param_id();

        if let Some(acc) = unsafe { repo.get::<EQUIP_PARAM_ACCESSORY_ST>(param_id) } {
            if EquipParam::as_accessory(acc).is_none() {
                continue;
            }

            for sp in accessory_speffects(acc) {
                out.insert(sp);
            }
        }
    }

    out
}


/// Called every frame for the main player. Automatically applies/removes
/// talisman SP effects when the player loads or the inventory changes.
pub fn tick(player: &mut PlayerIns) {
    let player_addr = player as *const PlayerIns as usize;
    let current_sps = collect_inventory_accessory_speffects(player);

    let mut guard = TALISMAN_STATE.lock().unwrap();

    match guard.as_mut() {
        None => {
            // First time seeing a player: apply all current SPs and store state.
            for sp in &current_sps {
                player.apply_speffect(*sp, true);
            }

            // Immediately max out for first-seen case.
            *guard = Some(TalismanState {
                player_addr,
                last_sps: current_sps,
                wait_until: None,
            });
            logger("New Player.\n");
        }
        Some(s) if s.player_addr != player_addr => {
            // Different player (reload / respawn / new main player).
            for sp in &s.last_sps {
                player.remove_speffect(*sp);
            }

            for sp in &current_sps {
                player.apply_speffect(*sp, true);
            }

            // Update stored state and schedule a delayed max-out instead of sleeping.
            s.player_addr = player_addr;
            s.last_sps = current_sps;
        }
        Some(s) => {
            // Same player: compute diffs and apply/remove immediately.
            let to_apply: Vec<i32> = current_sps.difference(&s.last_sps).copied().collect();
            let to_remove: Vec<i32> = s.last_sps.difference(&current_sps).copied().collect();

            for sp in &to_apply {
                player.apply_speffect(*sp, true);
            }
            for sp in &to_remove {
                player.remove_speffect(*sp);
            }

            s.last_sps = current_sps;
        }
    }
}

/// Return directory where current executable / DLL lives, or fallback to current dir.
fn dll_folder() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Logger that writes to a file next to the game's executable/DLL.
fn logger(text: &str) {
    if !DEBUG {
        return;
    }
    let path = dll_folder().join("PassiveTalismanLog.txt");

    // Prepare file for append
    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("talisman::logger: failed to open log file {}: {}", path.display(), e);
            return;
        }
    };

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if let Err(e) = writeln!(file, "Time: {}: {}", ts, text) {
        eprintln!("talisman::logger: write failed: {}", e);
    }
}