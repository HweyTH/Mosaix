# Per-monitor profiles matched by stored fingerprint content, not filename

**Status:** accepted

Per-monitor layout profiles (Tier 3 #20) live as separate files under `profiles/` next to `config.toml`, one file per saved topology. `topology_fingerprint()` (`mosaix-domain`, `display.rs`) is not filename-safe on Windows: its output contains `|` (a reserved NTFS character), plus spaces, `@`, `,`, and `=`. Rather than hashing or sanitizing the fingerprint into a filename, each profile file is named by a human-chosen (or auto-generated on first save, e.g. `profile-1.toml`) slug, and carries its real match key as a `fingerprint = "..."` field in its own content. `mosaix-config` matches the *active* profile by scanning every file in `profiles/` and comparing that field against the current `topology_fingerprint()` output -- never by filename.

## Considered Options

- **Hash the fingerprint into the filename** (e.g. `profiles/7f3a9c21.toml`): deterministic and collision-resistant, but filenames become meaningless to a human browsing the directory, and a stored fingerprint field would still be needed inside the file to guard against hash collisions -- so it doesn't actually remove the need for content-based matching, it just adds a hash on top.
- **Sanitize the fingerprint into a filename** (replace `| @ , =` and spaces with safe substitutes): keeps filenames visually derived from topology, but is lossy -- two distinct fingerprints could theoretically collide after substitution -- and produces ugly, hard-to-hand-edit filenames.
- **Human-assigned slug, fingerprint as content** (chosen): fully decouples the two concerns. Filenames stay short and human-meaningful (`home.toml`, `office.toml`); matching stays exact and collision-free because it compares the real fingerprint string, not a derived filename.

## Consequences

- Two profile files that both claim the same `fingerprint` value is a validation error (ambiguous which one is "the" active profile for that topology) -- caught by the same whole-directory validation pass as any other invalid config (ADR 0007).
- A profile file with no matching current topology just sits unused; nothing prunes it automatically (e.g. a laptop's "docked at office" profile stays on disk while working from home).
- When no profile's `fingerprint` matches the current topology, `mosaix-config` falls back to `config.toml`'s base settings rather than auto-creating a new profile file -- profiles are opt-in overrides you explicitly save, not something the agent generates on every unrecognized topology.
