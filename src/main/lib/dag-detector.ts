/**
 * DAG Markdown Detector
 *
 * Detects ```dag code blocks in agent output and automatically submits them
 * to the DAG scheduler for multi-agent orchestration.
 */

/**
 * Detect DAG markdown blocks in text
 *
 * Looks for code blocks with `dag` language tag:
 * ```dag
 * # Task: ...
 * ## Task A: ...
 * ```
 */
export function detectDagMarkdown(text: string): string | null {
  // Match ```dag ... ``` blocks
  const dagBlockRegex = /```dag\s*\n([\s\S]*?)```/g
  const matches: string[] = []

  let match
  while ((match = dagBlockRegex.exec(text)) !== null) {
    matches.push(match[1].trim())
  }

  // Return the last (most recent) DAG block, or null if none found
  return matches.length > 0 ? matches[matches.length - 1] : null
}

/**
 * Auto-submit detected DAG
 *
 * Called when a complete DAG block is detected in agent output.
 * Submits to the DAG scheduler and returns task IDs.
 */
export async function autoSubmitDag(markdown: string): Promise<string[]> {
  try {
    console.log("[DAG Detector] Detected DAG markdown, auto-submitting...")

    // Load native binding directly (same pattern as dag.ts router)
    const { dagSubmit } = require("../native-binding")

    // Call the NAPI function directly
    const result: string = await dagSubmit(markdown)
    const submittedTaskIds = JSON.parse(result) as string[]

    console.log("[DAG Detector] Submitted successfully:", submittedTaskIds)
    return submittedTaskIds
  } catch (error) {
    console.error("[DAG Detector] Failed to submit DAG:", error)
    return []
  }
}

/**
 * DAG Detector State
 *
 * Tracks accumulated text per session to detect complete DAG blocks.
 */
class DagDetectorState {
  private sessionTexts = new Map<string, string>()
  private submittedDags = new Set<string>() // Track submitted DAGs to avoid duplicates

  /**
   * Append text chunk to session buffer
   */
  appendChunk(sessionId: string, chunk: string): void {
    const current = this.sessionTexts.get(sessionId) || ""
    this.sessionTexts.set(sessionId, current + chunk)
  }

  /**
   * Check for new DAG blocks and auto-submit
   *
   * Returns array of newly detected DAG markdown blocks.
   */
  async checkAndSubmit(sessionId: string): Promise<string[]> {
    const text = this.sessionTexts.get(sessionId) || ""
    const dagMarkdown = detectDagMarkdown(text)

    if (!dagMarkdown) {
      return []
    }

    // Check if we already submitted this exact DAG
    const dagHash = this.hashDag(dagMarkdown)
    if (this.submittedDags.has(dagHash)) {
      return []
    }

    // Mark as submitted
    this.submittedDags.add(dagHash)

    // Auto-submit
    const taskIds = await autoSubmitDag(dagMarkdown)

    if (taskIds.length > 0) {
      console.log(`[DAG Detector] Auto-submitted ${taskIds.length} tasks for session ${sessionId}`)
    }

    return taskIds
  }

  /**
   * Clear session state
   */
  clearSession(sessionId: string): void {
    this.sessionTexts.delete(sessionId)
  }

  /**
   * Simple hash for deduplication
   */
  private hashDag(markdown: string): string {
    // Simple hash: just use length + first 100 chars
    return `${markdown.length}:${markdown.substring(0, 100)}`
  }
}

// Global singleton
export const dagDetector = new DagDetectorState()
