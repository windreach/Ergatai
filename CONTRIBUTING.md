# Contributing to Ergatai

Thank you for considering contributing to Ergatai! This document provides guidelines and information for contributors.

## Code of Conduct

By participating in this project, you agree to abide by our code of conduct: respect all participants and foster a friendly, inclusive environment.

## How to Contribute

### Reporting Bugs

1. First, check if the issue already exists in [Issues](https://github.com/windreach/Ergatai/issues)
2. If not, create a new issue including:
   - Clear title
   - Detailed description
   - Steps to reproduce
   - Expected vs actual behavior
   - Environment information (OS, Rust version, etc.)
   - Relevant log output

### Suggesting New Features

1. Create an issue first to discuss your idea
2. Explain the use case and expected behavior
3. Wait for maintainer feedback before starting development

### Submitting Code

#### Development Workflow

1. **Fork the repository**
   ```bash
   # Fork on GitHub, then clone
   git clone https://github.com/windreach/Ergatai.git
   cd ergatai
   ```

2. **Create a branch**
   ```bash
   git checkout -b feature/your-feature-name
   # or
   git checkout -b fix/issue-description
   ```

3. **Make your changes**
   ```bash
   # Build the project
   cargo build --workspace
   
   # Run tests
   cargo test --workspace
   
   # Check code quality
   cargo clippy --workspace -- -D warnings
   
   # Format code
   cargo fmt --all
   ```

4. **Commit your changes**
   
   Follow [Conventional Commits](https://www.conventionalcommits.org/) specification:
   ```bash
   git commit -m "feat: add new feature description"
   git commit -m "fix: fix issue description"
   git commit -m "docs: update documentation"
   git commit -m "test: add tests"
   git commit -m "refactor: refactor code"
   ```

5. **Push to your fork**
   ```bash
   git push origin feature/your-feature-name
   ```

6. **Create a Pull Request**
   - Create a PR on GitHub
   - Fill in the PR description explaining what and why
   - Link related issues (if any)
   - Wait for code review

#### Code Standards

- **Rust code style**: Follow `rustfmt` auto-formatting results
- **Clippy warnings**: All code must pass `cargo clippy -- -D warnings`
- **Test coverage**: New features require appropriate tests
- **Documentation**: Public APIs require documentation comments (`///`)
- **Error handling**: Use `?` operator and appropriate error types

#### Testing Guidelines

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p ergatai-core

# Run a single test
cargo test test_name

# Run integration tests
cargo test --test integration_test

# Show test output
cargo test -- --nocapture
```

### Improving Documentation

Documentation improvements are equally valuable! You can:
- Fix spelling and grammar errors
- Improve code examples
- Add missing explanations
- Translate documentation

## Development Environment Setup

### System Requirements

- Rust 1.70+ (latest stable recommended)
- Cargo (installed with Rust)
- Git

### Installing Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Building the Project

```bash
# Clone the repository
git clone https://github.com/yourusername/ergatai.git
cd ergatai

# Build all crates
cargo build --workspace

# Build release version
cargo build --release --workspace
```

### Running Examples

```bash
# Start the API server
cargo run --bin ergatai-server -- --port 3000

# In another terminal, run the example agent
cargo run -p simple-agent -- --port 8080 --agent-id my-agent --ergatai http://localhost:3000
```

## Project Structure

```
ergatai/
├── crates/
│   ├── ergatai-api/       # MCP server + REST API (binary: ergatai-server)
│   ├── ergatai-cli/       # CLI tool (binaries: ergatai, ega)
│   ├── ergatai-runtime/   # Agent runtime (discovery, injection, lifecycle)
│   ├── ergatai-nats/      # Embedded NATS server + JetStream streams
│   ├── ergatai-collab/    # Multi-agent collaboration (DAG scheduling)
│   ├── ergatai-dag/       # DAG parser, scheduler, dependency resolution
│   ├── ergatai-lock/      # Token-based file access control
│   ├── ergatai-agent/     # Agent config, discovery, hosted agents
│   ├── ergatai-core/      # Core library — business logic facade
│   ├── ergatai-error/     # Shared error types
│   └── ergatai-binary/    # Binary resources (rmux, nats-server)
├── examples/
│   └── simple-agent/      # Example MCP agent
├── docs/                  # Documentation
│   ├── getting-started/   # User guides
│   ├── guide/             # CLI, MCP configuration
│   ├── architecture/      # System design
│   └── dev/               # Internal development docs
├── assets/                # Static assets (logo, etc.)
── install.sh             # Installation script
```

## Code Review Process

1. All changes require a Pull Request
2. At least one maintainer must review
3. CI checks must pass (build, test, clippy)
4. Reviewers may request changes - respond promptly

## License

By contributing code, you agree that your contributions will be licensed under the project's Apache 2.0 License.

## Getting Help

If you need help while contributing:
- Check [CLAUDE.md](CLAUDE.md) for project architecture
- Check [README.md](README.md) for project overview
- Create an issue to ask questions
- Review existing code for implementation patterns

## Acknowledgments

Thank you to all developers who contribute to Ergatai!

---

**Note**: This guide may be updated. Please check for the latest version regularly.
