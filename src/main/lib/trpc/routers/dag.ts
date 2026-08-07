import { z } from "zod"
import { publicProcedure, router } from "../index"
import { join } from "path"
import { app } from "electron"

// Load native binding — same pattern as acp.ts
function loadNativeBinding(): any {
  const appRoot = app.getAppPath()
  const candidates = [
    join(appRoot, "src/native-binding"),
    join(appRoot, "out/main/native-binding"),
    join(appRoot, "native-binding"),
  ]
  for (const p of candidates) {
    try {
      return require(p)
    } catch {}
  }
  throw new Error("Cannot find native-binding module")
}

const nativeBinding = loadNativeBinding()
const { dagSubmit, dagProgress, dagIsComplete, dagStatus, dagGetState } = nativeBinding

/**
 * DAG orchestration router — wraps the Rust DagScheduler NAPI bindings.
 *
 * Flow: frontend calls dag.submit(markdown) → Rust parses DAG, creates DagScheduler,
 * submits ready nodes via ACP sessions → frontend polls dag.progress/dagIsComplete
 * for status updates. Node completion is auto-detected by Rust when ACP sessions finish.
 */
export const dagRouter = router({
  /** Submit a DAG for execution. Markdown format parsed by Rust dag_parser. */
  submit: publicProcedure
    .input(z.object({ markdown: z.string() }))
    .mutation(async ({ input }) => {
      const result: string = await dagSubmit(input.markdown)
      // result is a JSON array of submitted task IDs
      return { submittedTaskIds: JSON.parse(result) as string[] }
    }),

  /** Get progress as 0.0–1.0 */
  progress: publicProcedure.query(async () => {
    const p: number = await dagProgress()
    return { progress: p }
  }),

  /** Check if all nodes are complete (Completed/Failed/Skipped) */
  isComplete: publicProcedure.query(async () => {
    const done: boolean = await dagIsComplete()
    return { complete: done }
  }),

  /** AI-friendly status text */
  status: publicProcedure.query(async () => {
    const s: string = await dagStatus()
    return { status: s }
  }),

  /** Full graph state as JSON */
  getState: publicProcedure.query(async () => {
    const raw: string = await dagGetState()
    return JSON.parse(raw)
  }),
})
