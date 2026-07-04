# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

This repository is currently empty — no code, dependencies, or tooling exist yet. This file will need to be
updated once the project's language/framework, build commands, and architecture are established.

## Project goal

The user has thousands of GB of files (documents, photos, memories, etc.) spread across external HDDs. The
data is in a messy state: duplicated files, duplicated-and-renamed files, and generally disorganized. The
intended purpose of this project is to build tooling that:

1. **Deduplicates** files across the storage, keeping the original copy — including all of its metadata
   (timestamps, EXIF/file attributes, etc.) — when discarding duplicates.
2. **Cleans up** the messy/renamed/duplicated structure into some organized layout.
3. **Builds a register/catalog** of everything in storage, so the user can search and locate data without
   manually digging through folders or even knowing in advance what exists.

## Working in this repo

Since there is no established codebase yet, do not invent build/test/lint commands or architecture — confirm
the intended tech stack and structure with the user before scaffolding, and update this file once real
commands and structure exist.
