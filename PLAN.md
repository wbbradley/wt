# Next Up

## Documentation, integration hardening, and completion audit

Finish product documentation and verify the complete normal/bare/global workflow end to end.

- Document installation, registration, unregistered-session onboarding, bare repositories, CRUD safety and confirmations, keys/action palette, shell setup/completion, selectors, config, GitHub/GHE authentication, rate/error behavior, and troubleshooting in `README.md`.
- Verify repository-level error isolation, progressive local/GitHub rendering, action confirmations, selection output, and terminal cleanup across integrated flows.
- Ensure `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` pass.
- Manually verify normal and bare workflows, progressive GitHub updates, CRUD confirmations and safeguards, terminal restoration, cancellation/failure behavior, and sourced Bash navigation.
- Audit every requirement preserved across the preceding phases and add/fix any missing coverage before declaring the product complete.
