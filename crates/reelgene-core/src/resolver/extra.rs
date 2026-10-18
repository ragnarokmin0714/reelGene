//! Where site resolvers are registered.
//!
//! Public builds register none: reelgene ships as a general-purpose downloader
//! (direct links, HLS playlists). Site support lives outside the public
//! repository, on a branch that changes only this file and adds its own
//! resolver modules, so it rebases onto main without conflicts. Keep it that
//! way: main never edits this file beyond this placeholder.

use super::Registry;

pub(super) fn register(_registry: &mut Registry) {}
