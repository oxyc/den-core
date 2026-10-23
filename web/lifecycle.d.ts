export const STARTUP_OPERATIONS: readonly [
  'merge',
  'doc_decode',
  'doc_name',
  'doc_merge',
  'capture',
  'episode_mark',
  'continue_entry',
  'episode_state_v4',
  'profiles_validate',
  'profiles_seed_preferences',
  'profile_policy_allows',
  'kid_title_approvals_validate',
  'title_state',
  'download_status',
];

export function isStartupOperation(op: string): boolean;
