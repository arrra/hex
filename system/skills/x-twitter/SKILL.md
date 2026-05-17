---
name: x-twitter
version: 2.0.0
description: "Read, search, and post on X (Twitter) via the official xdevplatform xmcp server (FastMCP wrapper over X API v2)."
---

# X (Twitter) via xmcp

Access X/Twitter through the `x-twitter` MCP server — official `xdevplatform/xmcp` FastMCP server running at `~/github.com/xdevplatform/xmcp/`, invoked per-session over stdio.

**Transport:** stdio (spawned by Claude Code per session). No daemon.
**Auth:** OAuth1 user tokens + Bearer (pre-provisioned in `.env`, browser flow skipped).
**Allowlist:** 29 tools (set via `X_API_TOOL_ALLOWLIST` in `.env`). Full X API has 130+; whitelist is kept tight to avoid context bloat. Edit the allowlist + restart to add tools.

## Available tools (by category)

### Reading (public data)
- `searchPostsRecent` — search recent posts (last 7 days). Accepts X query operators (`from:`, `has:media`, `lang:en`, `-is:retweet`, etc.).
- `searchUsers` — search users by query string.
- `getUsersByUsername` / `getUsersByUsernames` — look up a user by handle (one or many).
- `getUsersById` — look up a user by numeric ID.
- `getUsersMe` — the authenticated user's profile.
- `getUsersPosts` — recent posts from a given user ID.
- `getUsersTimeline` — authenticated user's home timeline.
- `getUsersMentions` — mentions of the authenticated user.
- `getPostsById` / `getPostsByIds` — fetch post(s) by ID. Use `tweet.fields=article,note_tweet,attachments,entities` for full article / long-form text.
- `getPostsLikingUsers` — users who liked a given post.
- `getPostsReposts` — reposts of a given post.
- `getUsersFollowers` / `getUsersFollowing` — social graph for a user ID.
- `getTrendsByWoeid` — trending topics for a place (WOEID, e.g. `1` = worldwide, `23424977` = US).

### Writing (user auth required)
- `createPosts` — post a new tweet. Supports reply, quote, poll, media (via `media_ids`).
- `deletePosts` — delete one of your own posts.
- `likePost` / `unlikePost` — engage.
- `repostPost` / `unrepostPost` — retweet / undo.
- `followUser` / `unfollowUser` — follow graph.

### Bookmarks (OAuth2 required — see note below)
- `getUsersBookmarks` — fetch bookmarked posts.
- `getUsersBookmarkFolders` / `getUsersBookmarksByFolderId` — folder-scoped bookmarks.
- `createUsersBookmark` / `deleteUsersBookmark` — add/remove.

## Common patterns

### Read a post by URL
`getPostsById(id="1346889436626259968", tweet_fields=["text","author_id","public_metrics","created_at","note_tweet","article"])`

### Search someone's recent posts
`searchPostsRecent(query="from:karpathy", max_results=10)`

### Get user → timeline
```
user = getUsersByUsername(username="elonmusk")
getUsersPosts(id=user.data.id, max_results=20)
```

### Post a reply
`createPosts(text="your reply", reply={"in_reply_to_tweet_id": "1234..."})`

### Quote
`createPosts(text="your commentary", quote_tweet_id="1234...")`

## Reading articles (long-form X Articles)

Request the article field explicitly:

```
getPostsById(
    id="<post_id>",
    tweet_fields=["text","note_tweet","attachments","entities","article"],
    expansions=["article.cover_media","article.media_entities"]
)
```

Response includes:
- `data.article.plain_text` — full article body
- `data.article.title`
- `data.article.entities.code` — code blocks
- `data.article.preview_text`
- `data.article.cover_media`
- `data.article.media_entities`

## Rate limits (Basic tier)

- `searchPostsRecent`: ~40k requests/month
- `getPostsById`: ~450 requests per 15 min
- `createPosts`: ~100 per 24h on Free tier, higher on Basic

## Server config

- Repo: `~/github.com/xdevplatform/xmcp/`
- Entry point: `.venv/bin/python server.py`
- Credentials: `~/github.com/xdevplatform/xmcp/.env` (perms 600, git-ignored)
- Transport: `MCP_TRANSPORT=stdio`
- Local patch: `build_oauth1_client()` uses pre-provisioned `X_OAUTH_ACCESS_TOKEN`/`X_OAUTH_ACCESS_TOKEN_SECRET` from env to skip the OAuth1 browser flow (required for daemon-style use).
- Local patch: `main()` supports `MCP_TRANSPORT=stdio` in addition to default HTTP.

## Expanding the allowlist

Full tool catalog: see `~/github.com/xdevplatform/xmcp/README.md` (section "Available tool calls"). To add tools:

1. Edit `X_API_TOOL_ALLOWLIST` in `.env` (comma-separated)
2. Next Claude Code session picks it up automatically (stdio spawn)

## Bookmarks via OAuth2 (optional)

The OAuth1 user tokens above cover post/like/follow. Bookmarks need OAuth2 user context. To enable:

1. Add `CLIENT_ID` / `CLIENT_SECRET` to `.env` (from X Developer Portal → OAuth 2.0 settings).
2. `cd ~/github.com/xdevplatform/xmcp && .venv/bin/python generate_authtoken.py` — follow the prompt, authorize in browser.
3. Paste the returned access token into `X_OAUTH_ACCESS_TOKEN` in `.env`.

Until this is done, the bookmark tools will return auth errors. (Bookmarks are also accessible via Playwright — see `reference_x_tools.md` in memory.)

## Migration history

Swapped from community `Infatoshi/x-mcp` (Node stdio server) to official `xdevplatform/xmcp` (Python FastMCP over X API OpenAPI) on 2026-04-17. See `me/decisions/x-mcp-official-swap-2026-04-17.md`. Tool names changed: `get_tweet` → `getPostsById`, `search_tweets` → `searchPostsRecent`, `post_tweet` → `createPosts`, etc.
