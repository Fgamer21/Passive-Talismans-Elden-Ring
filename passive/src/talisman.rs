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
use std::fmt::Display;

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

    // Get FD4ParamRepository singleton.
    let repo = unsafe { FD4ParamRepository::instance().unwrap() };

    // Access PlayerGameData then the EquipInventoryData contained within it.
    let pg = player.player_game_data.as_ref();
    let inv = &pg.equipment.equip_inventory_data.items_data;

    for entry in InventoryItemsData::items(inv) {
        if entry.item_id.category() != ItemCategory::Accessory {
            continue;
        }

        // `entry.item_id.param_id()` returns a plain `u32` (ItemId -> param id).
        let param_id = entry.item_id.param_id();

        // Lookup accessory param row for the item param id.
        if let Some(acc) = repo.get::<EQUIP_PARAM_ACCESSORY_ST>(param_id) {
            // Defensive: sanity-check that this really "is" an accessory struct
            // (should always be true when we checked the category, but keep it safe).
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

/// Ensure exhaustable and proc-status values are set to their max after
/// talisman effects have been applied. This makes the player's current HP/FP/Stamina
/// and proc-status buildup (poison/rot/bleed/death/frost/sleep/madness) full.
pub fn max_out_exhaustables_and_statuses(player: &mut PlayerIns) {
        player.chr_ins.module_container.data.hp = player.chr_ins.module_container.data.max_hp;
        player.chr_ins.module_container.data.fp = player.chr_ins.module_container.data.max_fp;

         // Fill proc-status timers (poison, rot, bleed, death, frost, sleep, madness)
         // to their per-status max so "buildup" is considered full.
        for i in 0..player.player_game_data.proc_status_timer_max.len() {
            player.player_game_data.proc_status_timers[i] = player.player_game_data.proc_status_timer_max[i];
        }
        logger("Maxed out exhaustables and statuses.\n");
        logger(&player.chr_ins.module_container.data.fp.to_string());
        logger(&player.chr_ins.module_container.data.max_fp.to_string());
    }


/// Called every frame for the main player. Automatically applies/removes
/// talisman SP effects when the player loads or the inventory changes.
pub fn tick(player: &mut PlayerIns) {
    let player_addr = player as *const PlayerIns as usize;
    let current_sps = collect_inventory_accessory_speffects(player);

    let mut guard = TALISMAN_STATE.lock().unwrap();

    // If a delayed max-out is pending and its deadline passed, run it now.
    if let Some(state) = guard.as_mut() {
        if let Some(deadline) = state.wait_until {
            if Instant::now() >= deadline {
                // best-effort: call max out on the current player instance
                max_out_exhaustables_and_statuses(player);
                state.wait_until = None;
                logger("Delayed max_out executed.\n");
            }
        }
    }

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
            s.wait_until = Some(Instant::now() + Duration::from_millis(2000));
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