"""Format user records for display and export."""


def format_for_display(users):
    """Format user list for terminal display."""
    lines = []
    for user in users:
        name = user.get("name", "Unknown")
        email = user.get("email", "")
        # Normalize name: strip, title case
        name = name.strip().title()
        # Normalize email: strip, lowercase
        email = email.strip().lower()
        # Validate email has @
        if "@" not in email:
            email = "invalid"
        lines.append(f"{name} <{email}>")
    return "\n".join(lines)


def format_for_csv(users):
    """Format user list as CSV string."""
    rows = ["name,email"]
    for user in users:
        name = user.get("name", "Unknown")
        email = user.get("email", "")
        # Normalize name: strip, title case
        name = name.strip().title()
        # Normalize email: strip, lowercase
        email = email.strip().lower()
        # Validate email has @
        if "@" not in email:
            email = "invalid"
        rows.append(f"{name},{email}")
    return "\n".join(rows)


def format_for_json(users):
    """Format user list as list of normalized dicts."""
    result = []
    for user in users:
        name = user.get("name", "Unknown")
        email = user.get("email", "")
        # Normalize name: strip, title case
        name = name.strip().title()
        # Normalize email: strip, lowercase
        email = email.strip().lower()
        # Validate email has @
        if "@" not in email:
            email = "invalid"
        result.append({"name": name, "email": email})
    return result
