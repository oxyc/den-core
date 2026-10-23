/** Operations available before deferred action policy has loaded. */
export const STARTUP_OPERATIONS = Object.freeze([
  'merge',
  'doc_decode',
  'doc_name',
  'doc_merge',
  'capture',
  'issue',
  'episode_mark',
  'continue_entry',
  'episode_state_v4',
  'season_state_v4',
  'profiles_validate',
  'profiles_seed_preferences',
  'profile_policy_allows',
  'kid_title_approvals_validate',
  'title_state',
  'download_status',
]);

const startup = new Set(STARTUP_OPERATIONS);

/** Whether a request can run through the startup module without loading deferred policy. */
export function isStartupOperation(op) {
  return startup.has(op);
}
