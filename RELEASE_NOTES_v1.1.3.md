# Music163bot-Rust v1.1.3 Release Notes

## Summary

v1.1.3 enhances FLAC metadata handling to match the original project's behavior and adds a new admin command for bulk cache management.

## New Features

### `/clearallcache` - Bulk Cache Management (Admin Only)
- New admin command to clear all cached songs from database
- Two-step confirmation process to prevent accidental deletion
  - Step 1: `/clearallcache` - Shows confirmation prompt
  - Step 2: `/clearallcache confirm` - Executes the deletion
- Displays the number of records deleted after completion
- Operation logging for admin actions

## FLAC Metadata Improvements

### Complete Vorbis Comment Tags
- Added missing text metadata fields to FLAC files:
  - `TITLE`: Song name
  - `ALBUM`: Album name
  - `ARTIST`: Artist/performer name
  - `DESCRIPTION`: 163 key placeholder (Music ID)
- Previously only embedded album artwork; now includes full metadata like the original project

### High-Resolution Album Art Embedding
- Changed album art handling to use original high-resolution images (e.g., 1500×1500)
- Dual cover download system:
  - **Original resolution** for embedding in audio files (FLAC/MP3)
  - **320×320 thumbnail** for Telegram display
- Updated picture description from "Album Cover" to "Front cover" for consistency
- Significantly improved embedded artwork quality (e.g., ~192 KiB vs ~19 KiB)

## Technical Details

### Metadata Processing
- Both disk and memory storage modes support complete FLAC metadata writing
- Uses `metaflac` crate's `set_vorbis()` method for Vorbis Comment tags
- Parallel download of original and resized artwork for optimal performance
- MP3 files also benefit from original high-resolution artwork embedding

### Code Quality
- Fixed all clippy pedantic warnings
- Improved error message formatting with inline variables
- Modernized pattern matching with `let...else` syntax

## Backward Compatibility

All existing configurations remain valid. FLAC files will now include complete metadata and high-resolution artwork automatically.

## Configuration

No configuration changes required. The improvements work with existing settings.
