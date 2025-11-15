# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/fdiakh/slurm-spank-rs/compare/v0.4.0...v0.4.1) - 2025-11-15

### Other

- add release-plz workflow
- rename main branch
- update integration tests to Slurm 25.11
- fix warning from newer rustc

## [0.4.0](https://github.com/fdiakh/slurm-spank-rs/compare/v0.3.0...v0.4.0) - 2025-04-23

### Added

- [**breaking**] pass a mutable reference to self to setup()
- [**breaking**] pass a SpankHandle to report_error()

### Fixed

- *(examples)* fix renice example
- properly handle NULL zero-length arrays from Slurm

### Other

- Keep MSRV at 1.72 for now
- setup github actions
- upgrade dependencies
- update edition to 2021
