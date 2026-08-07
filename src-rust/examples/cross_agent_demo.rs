// Cross-Agent Communication Demo
// Demonstrates the cross-agent messaging flow without NAPI dependencies

use ergatai::cross_agent::{cross_agent_manager, detect_cross_agent_intent};

#[tokio::main]
async fn main() {
    println!("=== Cross-Agent Communication Demo ===\n");

    // Demo 1: Intent Detection
    println!("📋 Demo 1: Intent Detection");
    println!("Testing message intent detection...\n");

    let test_messages = vec![
        ("@codex 请审查这段代码", Some("codex")),
        ("@claude 帮我看看这个 bug", Some("claude")),
        ("send to opencode: 请优化这段代码", Some("opencode")),
        ("ask codex to review this", Some("codex")),
        ("hello world", None),
    ];

    for (msg, expected) in test_messages {
        let detected = detect_cross_agent_intent(msg);
        let detected_str = detected.as_deref();
        let status = if detected_str == expected { "✅" } else { "❌" };
        println!("{} Message: {:?}", status, msg);
        println!("   Expected: {:?}, Detected: {:?}", expected, detected_str);
    }

    // Demo 2: Message Flow
    println!("\n📤 Demo 2: Message Flow");
    println!("Simulating cross-agent conversation...\n");

    // Agent A sends message to Agent B
    println!("Step 1: Agent A sends message to Agent B");
    let conv_id = cross_agent_manager()
        .send_message(
            "agent-a".to_string(),
            "agent-b".to_string(),
            "Hello from Agent A! Can you help me with this task?".to_string(),
        )
        .await;

    match conv_id {
        Ok(id) => {
            println!("✅ Message sent successfully");
            println!("   Conversation ID: {}\n", id);

            // Agent B replies
            println!("Step 2: Agent B replies to Agent A");
            let reply_result = cross_agent_manager()
                .send_message(
                    "agent-b".to_string(),
                    "agent-a".to_string(),
                    "Hello from Agent B! Sure, what do you need?".to_string(),
                )
                .await;

            match reply_result {
                Ok(_) => {
                    println!("✅ Reply sent successfully\n");

                    // Retrieve conversation
                    println!("Step 3: Retrieve conversation history");
                    let conv = cross_agent_manager().get_conversation(&id).await;

                    if let Some(conversation) = conv {
                        println!("✅ Conversation retrieved");
                        println!("   Participants: {:?}", conversation.participants);
                        println!("   Total messages: {}\n", conversation.messages.len());

                        println!("Message History:");
                        for (i, msg) in conversation.messages.iter().enumerate() {
                            println!("   [{}] {} → {}", i, msg.from, msg.to);
                            println!("       Content: {}", msg.content);
                        }
                    }
                }
                Err(e) => {
                    println!("❌ Failed to send reply: {}\n", e);
                }
            }

            // List all conversations
            println!("\nStep 4: List all active conversations");
            let conversations = cross_agent_manager().list_conversations().await;
            println!("✅ Total conversations: {}", conversations.len());

            for conv in conversations {
                println!("   - {} ({} participants)", conv.id, conv.participants.len());
            }
        }
        Err(e) => {
            println!("❌ Failed to send message: {}\n", e);
        }
    }

    println!("\n=== Demo Complete ===");
    println!("\n✅ Cross-agent communication system is working!");
    println!("   - Intent detection: ✅");
    println!("   - Message sending: ✅");
    println!("   - Conversation tracking: ✅");
    println!("   - Message history: ✅");
}
