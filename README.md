# Ryvus

Ryvus is a modern, modular, and scalable automation and data-orchestration framework designed for reliability, simplicity, and long-term evolution. This monorepo contains all foundational components required to build and run automation pipelines locally or within self-hosted environments.

At its core, Ryvus provides:

* A **predictable execution engine**
* A structured **flow orchestration layer**
* Strong **typing**, **async safety**, and **deterministic behavior**
* A lightweight and ergonomic development experience

Ryvus focuses on core orchestration and execution mechanics. Cloud services and CLI tools are not yet part of this repository and will be introduced in future phases.

---

## 🚀 Vision

Ryvus aims to make automation and workflow orchestration accessible, predictable, and powerful—without unnecessary complexity.

Key principles guiding the project:

* **Modularity**: Each part of the system exists in its own crate, with clear boundaries.
* **Predictability**: Deterministic execution models and consistent error handling.
* **Developer-first ergonomics**: Minimal boilerplate, intuitive APIs, strong safety.
* **Scalability by design**: Data structures and architecture built for future distributed execution.
* **Ecosystem-ready**: Foundation designed to evolve without breaking stability.

Plugins, cloud orchestration, distributed runners, and advanced integrations will be layered on top once the foundations are fully stable.

---

## 📦 Repository Structure

All crates are organized inside a unified workspace at `crates/`.

```
ryvus/
│
├── Cargo.toml                 # Workspace definition
│
└── crates/
    ├── ryvus/                 # Umbrella crate (public entrypoint)
    ├── ryvus-core/            # Core shared types, primitives, and utilities
    ├── ryvus-engine/          # Execution engine and action runtime
    └── ryvus-flow/            # Pipeline and flow orchestration
```

### **Umbrella Crate (`ryvus`)**

External consumers depend on this single crate:

```toml
[dependencies]
ryvus = { git = "https://github.com/your-org/ryvus" }
```

It re-exports:

* `ryvus::core`
* `ryvus::engine`
* `ryvus::flow`

This keeps the external API clean and stable, independent of internal crate layout.

---

## 🔧 Crate Overview

### **ryvus-core**

Contains the essential building blocks:

* ExecutionContext
* ActionResult
* Error and result types
* Metric and metadata structures
* Core traits shared across the system

This crate is intentionally minimal and dependency-light.

### **ryvus-engine**

Implements the runtime responsible for:

* Executing actions
* Managing resolvers
* Handling hooks
* Step lifecycle management

It prioritizes correctness, async behavior, and clear control flow.

### **ryvus-flow**

Defines the high-level orchestration model:

* Pipeline definitions
* Steps and control structures (`next_when`, `else`, routing)
* JSONPath condition evaluation
* Flow-level utilities

The flow crate sits above the engine, turning low-level execution into structured workflows.

### **ryvus (umbrella)**

A convenience entrypoint that unifies all internal crates under one public-facing facade.

---

## 🧪 Example: Simple `steps.json`

Below is a minimal example of a Ryvus flow defined in JSON. This demonstrates how a simple pipeline with two steps might look:

```json
{
  "name": "example_pipeline",
  "steps": [
    {
      "id": "fetch_user",
      "action": "http.get",
      "config": {
        "url": "https://api.example.com/user/123"
      },
      "next": "process_user"
    },
    {
      "id": "process_user",
      "action": "transform.json",
      "config": {
        "select": "$.data.name"
      }
    }
  ]
}
```

This example shows two steps:

* **fetch_user** — Calls an HTTP GET endpoint.
* **process_user** — Extracts the user's name using a JSON selection.

More complex routing, conditions, and branching will be built on top of this structure as the project evolves.

---

## 🛠 Development

### **Prerequisites**

* Rust 1.75+ (stable)
* Cargo

### **Build everything**

```
cargo build --workspace
```

### **Run tests**

```
cargo test --workspace
```

### **Formatting & linting**

```
cargo fmt --all
cargo clippy --workspace --all-targets
```

---

## 🌱 Roadmap Foundation

(Future features—**not yet implemented**—but guiding the architecture)

* Expanded flow control patterns
* Stronger execution reporting and introspection
* Local runner improvements
* Configurable serialization and storage strategies

Cloud execution, distributed runners, and CLI tooling will be introduced only after the core engine and flow layers reach maturity.

---

## 🤝 Contributing

Contributions of any size are welcome. Whether improving documentation, refining APIs, or adding tests—every addition helps.

For large changes, please open an issue first to discuss alignment with project direction.

---

## 📄 License

Ryvus is licensed under the MIT License.

---

## ❤️ Acknowledgements

Ryvus is inspired by the simplicity and power of workflow engines, orchestration frameworks, and event-driven systems. It aims to bring clarity and reliability to automation without the heavyweight overhead.

Thanks for being part of its early evolution.
