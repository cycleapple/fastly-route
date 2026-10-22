# Changelog

## 0.3.0

- Add Steam Store as a built-in service with Fastly scanning, three-round image download benchmarks, stability ranking and grouped hosts apply/restore.
- Cover store.fastly.steamstatic.com, cdn.fastly.steamstatic.com and shared.fastly.steamstatic.com; leave the store origin, authentication and game downloads unchanged.
- Include all three static hosts' DNS addresses in default-route comparisons and preserve existing users' service selections.
- Document public, login-free downloads and updates.

## 0.2.0

- Add real three-round large-file benchmarks, stability ranking and default-route comparison.
- Add Fastly-validated custom domains, optional same-origin test URLs and persistent settings.
- Support all upstream Twitch hostnames, global scans, region selection and reusable scan cache.
- Add automatic release checks, private GitHub access, SHA-256 verification and in-app executable replacement with backup.
- Hide Windows consoles in debug/release and all child processes; add administrator restart and DNS flush.
- Fix compatibility with newer Rust float-literal checks in CI.

## 0.1.0

- Initial native Rust interface with HTTPS latency scan and managed hosts apply/restore.
