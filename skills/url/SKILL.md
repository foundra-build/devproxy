---
name: url
description: Use when you need to get the current project's devproxy HTTPS URL, check if the project is running behind devproxy, or need the proxy URL for testing, browser automation, or API calls. Triggers on "devproxy url", "proxy url", "get devproxy url", "what's my dev url", "current HTTPS url".
---

# Get devproxy URL

Run `devproxy get-url` to get the current project's proxy URL.

```bash
URL=$(devproxy get-url)
```

- **If the project is running**: prints the full HTTPS URL (e.g. `https://cool-penguin-myapp.mysite.dev`) to stdout and exits 0.
- **If the project is not running**: prints nothing and exits 1.

## Usage examples

Check if running and get URL:
```bash
if URL=$(devproxy get-url 2>/dev/null); then
  echo "Project is running at $URL"
else
  echo "Project is not running. Run 'devproxy up' first."
fi
```

Use with curl (macOS needs --resolve for DNS):
```bash
URL=$(devproxy get-url)
HOST=$(echo "$URL" | sed 's|https://||')
curl --resolve "$HOST:443:127.0.0.1" "$URL"
```

Use with Playwright/browser automation — just navigate to the URL directly.
