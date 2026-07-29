# Build the Astro site. Railway (and most platforms) use the repository root as
# the Docker build context, so paths here are written relative to the repo root
# (the site lives in website/). The root .dockerignore narrows the context to
# website/ plus the agentos SDK source (needed by the typedoc API-gen step), so
# the multi-GB monorepo is never uploaded. The shared docs theme
# (@rivet-dev/docs-theme) is git-cloned + built from GitHub at a pinned sha
# (the committed website deps are workspace:*/local for dev — we rewrite them
# to the clone here).
FROM node:22-alpine AS build
WORKDIR /app
RUN apk add --no-cache git && npm install -g pnpm@9

# Clone + build the shared docs theme at a pinned sha. The theme's dist/ is
# gitignored so we build it; its vendored @rivet-gg/components+icons come along.
ARG DOCS_THEME_SHA=450c498555135098c6a927adfdf13458be9be22a
# FA Pro token to generate the vendored @rivet-gg/icons dist. Set it as a Railway
# build variable (passed via --build-arg locally). It is only referenced in this
# discarded build stage — the final caddy image never contains it.
ARG FONTAWESOME_PACKAGE_TOKEN
# The ARG is already in this RUN's environment, so the icons generate reads
# FONTAWESOME_PACKAGE_TOKEN directly — we don't echo it into the build log.
RUN git clone https://github.com/rivet-dev/docs-theme.git /theme \
 && git -C /theme checkout "$DOCS_THEME_SHA" \
 && pnpm -C /theme install \
 && pnpm -C /theme/packages/theme/vendor/icons run gen:manifest \
 && pnpm -C /theme/packages/theme/vendor/icons run gen:vendor \
 && pnpm -C /theme/packages/theme build

# Copy the site source + the SDK source the typedoc step reads from ../packages.
COPY website/ ./
COPY tsconfig.base.json /tsconfig.base.json
COPY packages/agentos /packages/agentos
# The <CodeSnippet> docs embeds read example files from the embed root (/),
# resolved as /examples/<topic>/<file>.
COPY examples /examples

# Point the theme deps at the clone (committed deps are workspace:* for local
# dev) and drop the injected meta, then install + build.
RUN node -e "const fs=require('fs'),p=require('./package.json'),d=p.dependencies;d['@rivet-dev/docs-theme']='file:/theme/packages/theme';d['@rivet-gg/components']='file:/theme/packages/theme/vendor/components';d['@rivet-gg/icons']='file:/theme/packages/theme/vendor/icons';delete p.dependenciesMeta;fs.writeFileSync('./package.json',JSON.stringify(p,null,2))" \
 && pnpm install \
 && pnpm build

# Serve the static output. This is a static multi-page site (Astro/Starlight
# emits <path>/index.html), NOT an SPA — so resolve each page's index.html and
# serve the real 404.html on a miss (no SPA fallback to the landing page).
FROM caddy:alpine
COPY --from=build /app/dist /srv
RUN cat > /etc/caddy/Caddyfile <<'EOF'
:80 {
	root * /srv
	encode gzip

	# Canonicalize directory requests to a trailing slash BEFORE try_files maps
	# them to <path>/index.html. Pages like the TypeDoc /api index reference
	# their assets with relative URLs (e.g. "assets/style.css"). Without the
	# trailing slash the browser's base URL stays "/api", so those relatives
	# resolve to "/assets/style.css" at the site root and 404 (served as the
	# unstyled 404.html). Redirecting "/api" -> "/api/" makes the base URL
	# "/api/" so relative assets resolve to "/api/assets/...". The matcher only
	# fires when the bare path is itself a real directory on disk, so genuine
	# extension-less pages handled via {path}.html are unaffected.
	@dir_no_slash {
		not path */
		file {
			try_files {path}/index.html
		}
	}
	redir @dir_no_slash {path}/ 308

	try_files {path} {path}/index.html {path}.html
	file_server
	handle_errors {
		rewrite * /404.html
		file_server
	}
}
EOF
EXPOSE 80
