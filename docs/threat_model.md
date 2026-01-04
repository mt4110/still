# Threat Model

## Assets
- User data in `~/Library`
- System stability

## Threats
1. **Accidental Deletion**: Removing critical system files or app data.
   - *Mitigation*: Whitelist scope (`~/Library`), blacklist critical paths (future), quarantine instead of delete.
2. **Malicious Rules**: Importing a rule file that targets critical data.
   - *Mitigation*: Rules are strictly passive (filtering) for now. Future: Signed rulesets.
3. **Symlink Attacks**: Following a symlink to `/` or `/etc`.
   - *Mitigation*: `follow_links(false)` is hardcoded in scanner.
4. **Race Conditions**: File changes between scan and apply.
   - *Mitigation*: Check existence before move. Atomic `rename`.

## Trust Boundaries
- **Input**: User CLI arguments, local filesystem.
- **Output**: Plan file (JSON), Terminal UI, Vault directory.
