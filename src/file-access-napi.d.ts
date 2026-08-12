/* tslint:disable */
/* eslint-disable */

/* Manually created type definitions for file_access NAPI functions */
/* Auto-generation by NAPI-RS failed - this is a workaround */

/**
 * Initialize file access control system for a project
 */
export declare function fileAccessInit(
  projectId: string,
  projectRoot: string
): Promise<void>

/**
 * Register a system token for an agent
 */
export declare function fileAccessRegisterSystemToken(
  projectId: string,
  agentId: string,
  sessionId: string,
  projectRoot: string,
  ttlSecs: number,
  heartbeatIntervalSecs: number
): Promise<string>

/**
 * Request a file access token
 */
export declare function fileAccessRequestToken(
  projectId: string,
  agentId: string,
  sessionId: string,
  scope: string,
  mode: string,
  reason: string | null,
  ttlSecs: number,
  heartbeatIntervalSecs: number
): Promise<string>

/**
 * Acquire a file lock
 */
export declare function fileAccessAcquireLock(
  projectId: string,
  tokenId: string,
  filePath: string
): Promise<void>

/**
 * Release a file lock
 */
export declare function fileAccessReleaseLock(
  projectId: string,
  tokenId: string,
  filePath: string
): Promise<void>

/**
 * Read the latest version of a file (READ_LATEST semantics)
 */
export declare function fileAccessReadLatest(
  projectId: string,
  filePath: string
): Promise<Buffer>

/**
 * Create a snapshot of a file before modification
 */
export declare function fileAccessCreateSnapshot(
  projectId: string,
  filePath: string,
  agentId: string
): Promise<string>

/**
 * Mark a session as busy (task-aware heartbeat)
 */
export declare function fileAccessMarkBusy(
  projectId: string,
  sessionId: string,
  durationSecs: number
): Promise<void>

/**
 * Clear busy status for a session
 */
export declare function fileAccessClearBusy(
  projectId: string,
  sessionId: string
): Promise<void>

/**
 * Shutdown file access control system for a project
 */
export declare function fileAccessShutdown(projectId: string): Promise<void>

/**
 * Respond to an approval request from TypeScript
 */
export declare function fileAccessRespondApproval(
  requestId: string,
  approved: boolean,
  approvedBy: string,
  reason: string | null
): Promise<void>

/**
 * Upgrade a lock from READ to WRITE (release-then-acquire for deadlock safety)
 */
export declare function fileAccessUpgradeLock(
  projectId: string,
  tokenId: string,
  filePath: string
): Promise<void>

/**
 * Downgrade a lock from WRITE to READ
 */
export declare function fileAccessDowngradeLock(
  projectId: string,
  tokenId: string,
  filePath: string
): Promise<void>

/**
 * Check if a path is sensitive (requires ADMIN permission)
 */
export declare function fileAccessIsSensitivePath(
  projectId: string,
  filePath: string
): Promise<boolean>

/**
 * Check if a path is forbidden
 */
export declare function fileAccessIsForbiddenPath(
  projectId: string,
  filePath: string
): Promise<boolean>

/**
 * Reload project configuration
 */
export declare function fileAccessReloadConfig(
  projectId: string
): Promise<void>
