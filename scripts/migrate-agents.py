#!/usr/bin/env python3
"""Migrate agent definitions from Markdown to TOML format.

Reads AGENT-XX-NAME.md files from the PixelPerfekt agents directory
and generates AGENT-XX-NAME.toml files for sentinel-common.
"""

import os
import re
import sys
from pathlib import Path

SOURCE_DIR = Path("/work/company/pixelperfekt/agents")
TARGET_DIR = Path("/work/company/project-sentinel/config/agents")

# Room mappings per department/role
ROOM_MAP = {
    "CEO": "buero-ceo",
    "Geschaeftsfuehrer": "buero-ceo",
    "Head of Design": "buero-design",
    "Art Director": "buero-design",
    "Design": "buero-design",
    "Motion": "buero-design",
    "UX": "buero-design",
    "UI": "buero-design",
    "Head of Development": "buero-dev-1",
    "Tech Lead": "buero-dev-1",
    "Dev": "buero-dev-1",
    "Frontend": "buero-dev-1",
    "Backend": "buero-dev-1",
    "DevOps": "buero-dev-1",
    "Full-Stack": "buero-dev-1",
    "Entwickler": "buero-dev-1",
    "PM": "buero-pm",
    "Projektmanager": "buero-pm",
    "Sales": "buero-sales",
    "Marketing": "buero-marketing",
    "Admin": "buero-admin",
    "Office": "buero-admin",
    "Buchhaltung": "buero-admin",
    "IT": "buero-it",
    "Systemadmin": "buero-it",
    "Werkstud": "buero-dev-1",
    "Betriebsrat": "buero-betriebsrat",
    "Betriebspsych": "buero-betriebspsych",
    "Betriebsarzt": "buero-betriebsarzt",
}

# Department mappings
DEPT_MAP = {
    "CEO": "Geschaeftsfuehrung",
    "Geschaeftsfuehrer": "Geschaeftsfuehrung",
    "Design": "Design",
    "UX": "Design",
    "UI": "Design",
    "Motion": "Design",
    "Dev": "Entwicklung",
    "Frontend": "Entwicklung",
    "Backend": "Entwicklung",
    "DevOps": "Entwicklung",
    "Full-Stack": "Entwicklung",
    "Entwickler": "Entwicklung",
    "PM": "Projektmanagement",
    "Projektmanager": "Projektmanagement",
    "Sales": "Vertrieb",
    "Marketing": "Marketing",
    "Admin": "Verwaltung",
    "Office": "Verwaltung",
    "Buchhaltung": "Verwaltung",
    "IT": "IT",
    "Systemadmin": "IT",
    "Werkstud": "Entwicklung",
    "Betriebsrat": "Betriebsrat",
    "Betriebspsych": "Betriebsgesundheit",
    "Betriebsarzt": "Betriebsgesundheit",
}


def get_shift_set(agent_id: int) -> int:
    if 1 <= agent_id <= 15:
        return 1
    elif 16 <= agent_id <= 30:
        return 2
    elif 31 <= agent_id <= 45:
        return 3
    elif 46 <= agent_id <= 54:
        return 0
    else:
        raise ValueError(f"Invalid agent ID: {agent_id}")


def parse_big_five(content: str) -> dict:
    """Parse Big Five personality values from X/10 format."""
    result = {}
    patterns = {
        "openness": r"\*\*Offenheit:\*\*\s*(\d+)/10",
        "conscientiousness": r"\*\*Gewissenhaftigkeit:\*\*\s*(\d+)/10",
        "extraversion": r"\*\*Extraversion:\*\*\s*(\d+)/10",
        "agreeableness": r"\*\*Vertraeglichkeit:\*\*\s*(\d+)/10",
        "neuroticism": r"\*\*Neurotizismus:\*\*\s*(\d+)/10",
    }
    for key, pattern in patterns.items():
        match = re.search(pattern, content)
        if match:
            result[key] = round(int(match.group(1)) / 10.0, 1)
        else:
            print(f"  WARNING: {key} not found, defaulting to 0.5")
            result[key] = 0.5
    return result


def parse_name(content: str) -> str:
    match = re.search(r"\*\*Name:\*\*\s*(.+)", content)
    return match.group(1).strip() if match else ""


def parse_position(content: str) -> str:
    match = re.search(r"\*\*Position:\*\*\s*(.+)", content)
    return match.group(1).strip() if match else ""


def guess_department(filename: str, position: str) -> str:
    """Guess department from filename suffix and position."""
    name_upper = filename.upper()
    for key, dept in DEPT_MAP.items():
        if key.upper() in name_upper or key.upper() in position.upper():
            return dept
    return "Sonstige"


def guess_room(filename: str, position: str) -> str:
    """Guess favorite room from filename suffix and position."""
    name_upper = filename.upper()
    for key, room in ROOM_MAP.items():
        if key.upper() in name_upper or key.upper() in position.upper():
            return room
    return "buero-dev-1"


def parse_quirks(content: str) -> list:
    """Parse quirks/habits from the Ticks & Gewohnheiten section."""
    quirks = []
    in_section = False
    for line in content.split("\n"):
        if "Ticks" in line and "Gewohnheiten" in line:
            in_section = True
            continue
        if in_section:
            if line.startswith("##") or line.startswith("---"):
                break
            if line.startswith("- "):
                quirk = line[2:].strip()
                if quirk:
                    quirks.append(quirk)
    return quirks[:3] if quirks else ["Keine besonderen Ticks bekannt"]


def parse_bio(content: str) -> str:
    """Extract a compact bio from the Biografie section."""
    in_section = False
    bio_lines = []
    for line in content.split("\n"):
        if "Biografie" in line and "Hintergrund" in line:
            in_section = True
            continue
        if in_section:
            if line.startswith("##") or line.startswith("---"):
                break
            stripped = line.strip()
            if stripped:
                bio_lines.append(stripped)
    full_bio = " ".join(bio_lines)
    # Truncate to ~200 chars for TOML
    if len(full_bio) > 200:
        full_bio = full_bio[:197] + "..."
    return full_bio if full_bio else "Keine Biografie verfuegbar"


def has_coffee_preference(content: str) -> str:
    """Try to extract coffee preference."""
    lower = content.lower()
    if "kein kaffee" in lower or "nie kaffee" in lower or "keinen kaffee" in lower:
        return "kein Kaffee"
    if "tee" in lower and ("statt kaffee" in lower or "gruener tee" in lower or "gruenen tee" in lower):
        return "Tee"
    if "espresso" in lower:
        return "Espresso"
    if "cappuccino" in lower:
        return "Cappuccino"
    if "latte" in lower:
        return "Latte Macchiato"
    if "schwarz" in lower and "kaffee" in lower:
        return "schwarz"
    if "milch" in lower and "kaffee" in lower:
        return "mit Milch"
    return "Kaffee"


def guess_caffeine_tolerance(content: str, coffee_pref: str) -> float:
    """Estimate caffeine tolerance from content."""
    lower = content.lower()
    if "kein kaffee" in lower or coffee_pref == "kein Kaffee" or coffee_pref == "Tee":
        return 0.3
    if "4-5" in lower and "kaffee" in lower:
        return 0.8
    if "3" in lower and "kaffee" in lower:
        return 0.7
    if "espresso" in lower:
        return 0.7
    return 0.5


def guess_morning_person(content: str, shift_set: int) -> bool:
    """Guess if agent is a morning person."""
    lower = content.lower()
    if shift_set == 3:  # Spaetschicht
        return False
    if "fruehaufsteher" in lower or "morgenmensch" in lower:
        return True
    if "5:" in lower or "6:00" in lower or "6:30" in lower:
        return True
    if "nachtmensch" in lower or "nachteule" in lower:
        return False
    return shift_set == 1  # Fruehschicht = morning person


def guess_lunch_time(shift_set: int) -> str:
    """Guess lunch time based on shift."""
    if shift_set == 1:
        return "12:00"
    elif shift_set == 2:
        return "18:00"
    elif shift_set == 3:
        return "02:00"
    else:  # Sonder
        return "12:30"


def escape_toml_string(s: str) -> str:
    """Escape a string for TOML."""
    return s.replace("\\", "\\\\").replace('"', '\\"')


def generate_toml(agent_id: int, md_content: str, filename: str) -> str:
    """Generate TOML content from parsed Markdown."""
    name = parse_name(md_content)
    position = parse_position(md_content)
    department = guess_department(filename, position)
    shift_set = get_shift_set(agent_id)
    big_five = parse_big_five(md_content)
    bio = parse_bio(md_content)
    quirks = parse_quirks(md_content)
    coffee = has_coffee_preference(md_content)
    caffeine = guess_caffeine_tolerance(md_content, coffee)
    morning = guess_morning_person(md_content, shift_set)
    room = guess_room(filename, position)
    lunch = guess_lunch_time(shift_set)

    quirks_toml = ", ".join(f'"{escape_toml_string(q)}"' for q in quirks)

    return f"""[identity]
id = {agent_id}
name = "{escape_toml_string(name)}"
role = "{escape_toml_string(position)}"
department = "{escape_toml_string(department)}"
shift_set = {shift_set}

[personality]
openness = {big_five['openness']}
conscientiousness = {big_five['conscientiousness']}
extraversion = {big_five['extraversion']}
agreeableness = {big_five['agreeableness']}
neuroticism = {big_five['neuroticism']}
caffeine_tolerance = {caffeine}
morning_person = {"true" if morning else "false"}

[preferences]
favorite_room = "{room}"
coffee_preference = "{escape_toml_string(coffee)}"
lunch_time = "{lunch}"

[background]
bio = "{escape_toml_string(bio)}"
quirks = [{quirks_toml}]
"""


def main():
    if not SOURCE_DIR.exists():
        print(f"ERROR: Source directory not found: {SOURCE_DIR}")
        sys.exit(1)

    TARGET_DIR.mkdir(parents=True, exist_ok=True)

    md_files = sorted(SOURCE_DIR.glob("AGENT-*.md"))
    print(f"Found {len(md_files)} Markdown agent files")

    migrated = 0
    skipped = 0
    errors = 0

    for md_file in md_files:
        # Extract agent ID from filename
        match = re.match(r"AGENT-(\d+)-(.+)\.md", md_file.name)
        if not match:
            print(f"  SKIP: {md_file.name} (filename pattern mismatch)")
            skipped += 1
            continue

        agent_id = int(match.group(1))
        name_part = match.group(2)
        toml_name = f"AGENT-{agent_id:02d}-{name_part}.toml"
        toml_path = TARGET_DIR / toml_name

        # Skip existing files only if --force not given
        if toml_path.exists() and "--force" not in sys.argv:
            print(f"  SKIP: {toml_name} (already exists)")
            skipped += 1
            continue

        print(f"  Migrating: {md_file.name} -> {toml_name}")
        try:
            md_content = md_file.read_text(encoding="utf-8")
            toml_content = generate_toml(agent_id, md_content, md_file.name)
            toml_path.write_text(toml_content, encoding="utf-8")
            migrated += 1
        except Exception as e:
            print(f"  ERROR: {md_file.name}: {e}")
            errors += 1

    print(f"\nMigration complete: {migrated} migrated, {skipped} skipped, {errors} errors")
    print(f"Total TOML files: {len(list(TARGET_DIR.glob('AGENT-*.toml')))}")


if __name__ == "__main__":
    main()
