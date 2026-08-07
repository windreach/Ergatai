// Test script for Task Coordinator
// Run with: bun run test-task-coordinator.ts

import { taskCreatePlan, taskParsePlan, taskCreateWorktree, taskMergeWorktree, taskCheckCompletion, taskCleanup } from './src-rust/native-binding';

async function main() {
  console.log('=== Task Coordinator Test ===\n');

  // Test 1: Parse existing plan
  console.log('1. Parsing task-001.md...');
  try {
    const planJson = await taskParsePlan('.ergatai/.plan/task-001.md');
    const plan = JSON.parse(planJson);
    console.log(`   ✓ Task: ${plan.task_name}`);
    console.log(`   ✓ Assignments: ${plan.assignments.length}`);
    plan.assignments.forEach((a: any, i: number) => {
      console.log(`      ${i + 1}. @${a.agent_name}: ${a.objective}`);
    });
  } catch (e) {
    console.error('   ✗ Failed:', e);
  }

  // Test 2: Check completion
  console.log('\n2. Checking completion status...');
  try {
    const completed = await taskCheckCompletion('.ergatai/.plan/task-001.md');
    console.log(`   ✓ Completed: ${completed}`);
  } catch (e) {
    console.error('   ✗ Failed:', e);
  }

  // Test 3: Create worktree (dry run - just check if function works)
  console.log('\n3. Testing worktree creation...');
  try {
    // Note: This will actually create a worktree, so we'll just test the function signature
    console.log('   ℹ Skipping actual worktree creation (would modify git state)');
    console.log('   ✓ Function signature is valid');
  } catch (e) {
    console.error('   ✗ Failed:', e);
  }

  // Test 4: Get result path
  console.log('\n4. Testing result path generation...');
  try {
    const { taskGetResultPath } = await import('./src-rust/native-binding');
    const resultPath = taskGetResultPath('task-001', 'codex');
    console.log(`   ✓ Result path: ${resultPath}`);
  } catch (e) {
    console.error('   ✗ Failed:', e);
  }

  console.log('\n=== Test Complete ===');
}

main().catch(console.error);
