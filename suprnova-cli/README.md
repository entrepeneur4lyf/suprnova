# Suprnova CLI

A CLI tool for scaffolding Suprnova web applications.

## Installation

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

## Usage

### Create a new project

```bash
suprnova new myapp
```

This will interactively prompt you for:
- Project name
- Description
- Author

### Non-interactive mode

```bash
suprnova new myapp --no-interaction
```

### Skip git initialization

```bash
suprnova new myapp --no-git
```

## Generated Project Structure

```
myapp/
├── Cargo.toml
├── .gitignore
├── cmd/
│   └── main.rs
└── src/
    ├── lib.rs
    └── controllers/
        ├── mod.rs
        └── home.rs
```

## License

MIT
