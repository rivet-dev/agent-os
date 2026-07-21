# Browser Automation

Let agents read and search the web from an agentOS VM using Browserbase's cloud browser through the browse CLI — no local browser or sandbox required.

Agents can read and search the web with the [Browserbase](https://www.browserbase.com) `browse` CLI. The page loads in a real browser in Browserbase's cloud and comes back as clean content — the VM never runs a browser.

## Setup

1. **Create a Browserbase account**

   [Sign up](https://www.browserbase.com/sign-up) and grab your API key and project id from the [dashboard](https://www.browserbase.com/settings):

   ```bash
   export BROWSERBASE_API_KEY=bb_...
   export BROWSERBASE_PROJECT_ID=...
   ```

2. **Install**

   ```bash
   npm install @rivet-dev/agentos @agentos-software/pi @agentos-software/browserbase
   ```

3. **Add `browse` to the VM**

     Mount the [`browse` CLI skill](https://github.com/browserbase/stagehand/tree/main/packages/cli) into the agent's skills directory so it reaches for `browse` unprompted ([copy the skill folder from the example](https://github.com/rivet-dev/agentos/tree/main/examples/browserbase/skills)):

4. **Use it**

## Command reference

```bash
browse cloud fetch https://example.com   # retrieve a page as markdown
browse cloud search "web scraping tools" # search the web
browse cloud sessions list               # list cloud browser sessions
browse cloud projects list               # list Browserbase projects
```

The [interactive driver mode](https://docs.browserbase.com/integrations/skills/browse-cli) (`browse open`, `browse click`, …) is not supported inside the VM yet ([#1631](https://github.com/rivet-dev/agentos/issues/1631)). For interactive automation, run `browse` inside an external sandbox via [External Sandboxes](/docs/sandboxes).