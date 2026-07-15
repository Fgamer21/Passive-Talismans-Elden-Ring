use std::time::Duration;

use eldenring::{
    cs::{CSTaskGroupIndex, CSTaskImp, WorldChrMan},
    fd4::FD4TaskData,
    util::{system::wait_for_system_init},
};

use fromsoftware_shared::{FromStatic, program::Program, task};
use fromsoftware_shared::SharedTaskImpExt;
use fromsoftware_shared::singleton;
use eldenring::util::input;

mod talisman;

/// # Safety
/// This is exposed this way such that libraryloader can call it. Do not call this yourself.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn DllMain(_hmodule: u64, reason: u32) -> bool {
    // Exit early if we're not attaching a DLL
    if reason != 1 {
        return true;
    }

    std::thread::spawn(move || {
        wait_for_system_init(&Program::current(), Duration::MAX)
            .expect("Timeout waiting for system init");

        // Retrieve games task runner and register a task at frame begin.
        let cs_task = unsafe { CSTaskImp::instance().unwrap() };
        cs_task.run_recurring(
            |_: &FD4TaskData| {
                // Retrieve WorldChrMan
                let Ok(world_chr_man) = (unsafe { WorldChrMan::instance_mut() }) else {
                    return;
                };

                // Player reference
                let Some(main_player) = world_chr_man.main_player.as_mut() else {
                    return;
                };

                // Each frame: update talisman effects for the main player.
                talisman::tick(main_player);

                // Keep debug key to check functions (O)
                if input::is_key_pressed(0x4F) {
                }

            },
            CSTaskGroupIndex::ChrIns_PostPhysics,
        );
    });

    true
}