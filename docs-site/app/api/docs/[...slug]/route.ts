import { NextRequest, NextResponse } from "next/server";
import { readFile, readdir, stat } from "fs/promises";
import { join } from "path";

// Content directory relative to project root
const CONTENT_DIR = join(process.cwd(), "content");

/**
 * Strip frontmatter from MDX/markdown content
 * Frontmatter is the YAML block between --- markers at the start
 */
function stripFrontmatter(content: string): string {
  const frontmatterRegex = /^---\s*\n[\s\S]*?\n---\s*\n/;
  return content.replace(frontmatterRegex, "").trim();
}

/**
 * Extract frontmatter metadata from MDX content
 */
function extractFrontmatter(content: string): Record<string, string> {
  const match = content.match(/^---\s*\n([\s\S]*?)\n---/);
  if (!match) return {};

  const frontmatter: Record<string, string> = {};
  const lines = match[1].split("\n");
  for (const line of lines) {
    const [key, ...valueParts] = line.split(":");
    if (key && valueParts.length) {
      frontmatter[key.trim()] = valueParts.join(":").trim();
    }
  }
  return frontmatter;
}

/**
 * List all available documentation files
 */
async function listDocs(dir: string = CONTENT_DIR, prefix: string = ""): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true });
  const docs: string[] = [];

  for (const entry of entries) {
    // Skip meta files and hidden files
    if (entry.name.startsWith("_") || entry.name.startsWith(".")) continue;

    const fullPath = join(dir, entry.name);
    const relativePath = prefix ? `${prefix}/${entry.name}` : entry.name;

    if (entry.isDirectory()) {
      docs.push(...(await listDocs(fullPath, relativePath)));
    } else if (entry.name.endsWith(".mdx") || entry.name.endsWith(".md")) {
      // Convert filename to URL path (remove extension)
      const urlPath = relativePath.replace(/\.(mdx?|md)$/, "");
      docs.push(urlPath);
    }
  }

  return docs;
}

/**
 * GET /api/docs/[...slug]
 *
 * Serves raw markdown content for AI agents and programmatic access.
 *
 * Special paths:
 * - /api/docs/_index → List all available docs (JSON)
 * - /api/docs/_all   → All docs concatenated (Markdown)
 *
 * Examples:
 * - /api/docs/index.md → Raw markdown for homepage
 * - /api/docs/mission-api → Raw markdown (extension optional)
 */
export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ slug: string[] }> }
) {
  const { slug } = await params;
  const path = slug.join("/");

  // Special route: list all docs
  if (path === "_index") {
    try {
      const docs = await listDocs();
      return NextResponse.json({
        description: "Open Agent Documentation Index",
        docs: docs.map((doc) => ({
          path: doc,
          url: `/api/docs/${doc}`,
          html_url: `/${doc === "index" ? "" : doc}`,
        })),
      });
    } catch {
      return NextResponse.json({ error: "Failed to list docs" }, { status: 500 });
    }
  }

  // Special route: all docs concatenated
  if (path === "_all") {
    try {
      const docs = await listDocs();
      const contents: string[] = [
        "# Open Agent - Complete Documentation",
        "",
        "> This file contains all documentation concatenated for AI agent consumption.",
        "",
        "---",
        "",
      ];

      for (const docPath of docs) {
        const filePath = join(CONTENT_DIR, `${docPath}.mdx`);
        try {
          const content = await readFile(filePath, "utf-8");
          const frontmatter = extractFrontmatter(content);
          const markdown = stripFrontmatter(content);

          contents.push(`# ${frontmatter.title || docPath}`);
          contents.push("");
          if (frontmatter.description) {
            contents.push(`> ${frontmatter.description}`);
            contents.push("");
          }
          contents.push(markdown);
          contents.push("");
          contents.push("---");
          contents.push("");
        } catch {
          // Try .md extension as fallback
          try {
            const mdPath = join(CONTENT_DIR, `${docPath}.md`);
            const content = await readFile(mdPath, "utf-8");
            contents.push(stripFrontmatter(content));
            contents.push("");
            contents.push("---");
            contents.push("");
          } catch {
            // Skip if file not found
          }
        }
      }

      return new NextResponse(contents.join("\n"), {
        headers: {
          "Content-Type": "text/markdown; charset=utf-8",
          "Cache-Control": "public, max-age=3600",
          "X-Content-Type-Options": "nosniff",
        },
      });
    } catch {
      return NextResponse.json({ error: "Failed to compile docs" }, { status: 500 });
    }
  }

  // Regular doc request - normalize path
  let normalizedPath = path.replace(/\.md$/, ""); // Strip .md extension if present

  // Try to find the file
  const possiblePaths = [
    join(CONTENT_DIR, `${normalizedPath}.mdx`),
    join(CONTENT_DIR, `${normalizedPath}.md`),
    join(CONTENT_DIR, normalizedPath, "index.mdx"),
    join(CONTENT_DIR, normalizedPath, "index.md"),
  ];

  let content: string | null = null;
  let foundPath: string | null = null;

  for (const filePath of possiblePaths) {
    try {
      const stats = await stat(filePath);
      if (stats.isFile()) {
        content = await readFile(filePath, "utf-8");
        foundPath = filePath;
        break;
      }
    } catch {
      // File doesn't exist, try next
    }
  }

  if (!content || !foundPath) {
    return NextResponse.json(
      {
        error: "Document not found",
        path: normalizedPath,
        suggestion: "Use /api/docs/_index to list available documents",
      },
      { status: 404 }
    );
  }

  // Extract metadata and strip frontmatter
  const frontmatter = extractFrontmatter(content);
  const markdown = stripFrontmatter(content);

  // Build response with optional metadata header
  const includeMetadata = request.nextUrl.searchParams.get("metadata") === "true";
  let responseContent = markdown;

  if (includeMetadata && (frontmatter.title || frontmatter.description)) {
    const metaLines = [];
    if (frontmatter.title) metaLines.push(`# ${frontmatter.title}`);
    if (frontmatter.description) metaLines.push(`> ${frontmatter.description}`);
    if (metaLines.length) {
      responseContent = metaLines.join("\n") + "\n\n" + markdown;
    }
  }

  return new NextResponse(responseContent, {
    headers: {
      "Content-Type": "text/markdown; charset=utf-8",
      "Cache-Control": "public, max-age=3600",
      "X-Content-Type-Options": "nosniff",
      "X-Doc-Title": frontmatter.title || normalizedPath,
      "X-Doc-Path": normalizedPath,
    },
  });
}
