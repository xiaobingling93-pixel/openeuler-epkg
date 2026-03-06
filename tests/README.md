Tests shall be able to run freely inside AI agent sandbox, so that
AI agent can freely reproduce-debug-fix bugs in a fully automated loop.

Test scripts shall follow below common principles:

- Supports debug mode with -d/-dd/-ddd flags
- do not 'set -e'
- avoid lots of >/dev/null: we are testing! so preserve context and error info
- Assumes epkg is already installed (except for the more heavier tests/e2e/ which covers install-from-scratch tests and tests that may pollute host os)
- (Re-)creates new env with non-random names for various testing
- Run tests with 'timeout' prefix and '-y|--assume-yes' for automation w/o blocking
- Log to /tmp/ files with non-random names with backup file for grep based problem analyze and comparison with history behavior
- Leaves the env for human/agent debug; i.e. do not remove env in the end, but remove it in the beginning, before create, if it already exists
