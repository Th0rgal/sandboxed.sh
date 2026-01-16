import { NextRequest, NextResponse } from "next/server";

/**
 * Known LLM/AI user agent patterns
 * These agents get raw markdown automatically
 */
const LLM_USER_AGENTS = [
  "ChatGPT-User",     // OpenAI ChatGPT browsing
  "GPTBot",           // OpenAI crawler
  "Claude-Web",       // Anthropic (if they add browsing)
  "ClaudeBot",        // Anthropic crawler
  "PerplexityBot",    // Perplexity AI
  "Applebot",         // Apple Intelligence/Siri
  "cohere-ai",        // Cohere
  "anthropic-ai",     // Anthropic
  "Google-Extended",  // Google AI (Bard/Gemini)
  "CCBot",            // Common Crawl (used by many LLMs)
];

/**
 * Check if user agent is an LLM/AI agent
 */
function isLLMUserAgent(userAgent: string | null): boolean {
  if (!userAgent) return false;
  return LLM_USER_AGENTS.some(bot =>
    userAgent.toLowerCase().includes(bot.toLowerCase())
  );
}

/**
 * Proxy to serve raw markdown for AI agents
 *
 * Routes to raw markdown API when:
 * 1. URL ends with .md extension (e.g., /setup.md)
 * 2. Accept header includes text/markdown
 * 3. Query param ?format=md is present
 * 4. User-Agent is a known LLM (ChatGPT, GPTBot, PerplexityBot, etc.)
 *
 * This allows AI agents to fetch documentation as raw markdown
 * while browsers get the rendered HTML version.
 *
 * Note: In Next.js 16+, middleware.ts was renamed to proxy.ts
 * See: https://nextjs.org/docs/messages/middleware-to-proxy
 */
export function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  // Skip API routes, static files, and Next.js internals
  if (
    pathname.startsWith("/api/") ||
    pathname.startsWith("/_next/") ||
    pathname.startsWith("/static/") ||
    pathname.includes(".")  && !pathname.endsWith(".md") // Has extension but not .md
  ) {
    return NextResponse.next();
  }

  const userAgent = request.headers.get("User-Agent");

  // Check if this is a docs page request that wants markdown
  const wantsMarkdown =
    pathname.endsWith(".md") ||
    request.headers.get("Accept")?.includes("text/markdown") ||
    request.nextUrl.searchParams.get("format") === "md" ||
    isLLMUserAgent(userAgent);

  if (wantsMarkdown) {
    // Normalize the path (remove .md extension if present)
    let docPath = pathname.replace(/\.md$/, "");

    // Handle root path
    if (docPath === "" || docPath === "/") {
      docPath = "/index";
    }

    // Preserve query params except format
    const url = new URL(request.url);
    url.pathname = `/api/docs${docPath}`;
    url.searchParams.delete("format");

    // Rewrite to the API route (internal redirect, URL doesn't change for client)
    return NextResponse.rewrite(url);
  }

  return NextResponse.next();
}

// Only run proxy on relevant paths
export const config = {
  matcher: [
    /*
     * Match all paths except:
     * - API routes (already handled)
     * - Static files with extensions (images, css, js, etc.)
     * - Next.js internals
     */
    "/((?!api|_next/static|_next/image|favicon.ico|.*\\.[^m][^d]$).*)",
  ],
};
