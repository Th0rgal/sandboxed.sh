import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { Head } from "nextra/components";
import { ThemeProvider } from "@/components/theme-provider";
import "./globals.css";

export const viewport: Viewport = {
  themeColor: [
    { media: "(prefers-color-scheme: light)", color: "#ffffff" },
    { media: "(prefers-color-scheme: dark)", color: "#0c0b0a" },
  ],
  colorScheme: "light dark",
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
};

export const metadata: Metadata = {
  metadataBase: new URL("https://openagent.thomas.md"),
  title: {
    default: "Open Agent | Cloud Orchestrator for AI Coding Agents",
    template: "%s | Open Agent",
  },
  description:
    "Self-hosted cloud orchestrator for AI coding agents (Claude Code & OpenCode). Mission orchestration, workspace management, and library sync.",
  applicationName: "Open Agent",
  generator: "Next.js",
  keywords: [
    "ai agent",
    "opencode",
    "claude",
    "automation",
    "orchestration",
    "workspace",
    "mcp",
    "model context protocol",
  ],
  authors: [{ name: "Thomas Marchand", url: "https://thomas.md" }],
  creator: "Thomas Marchand",
  publisher: "Open Agent",
  robots: {
    index: true,
    follow: true,
  },
  twitter: {
    card: "summary_large_image",
    title: "Open Agent",
    description:
      "Self-hosted cloud orchestrator for AI coding agents (Claude Code & OpenCode). Mission orchestration, workspace management, and library sync.",
    creator: "@music_music_yo",
    images: ["/og-image.png"],
  },
  openGraph: {
    type: "website",
    locale: "en_US",
    url: "https://openagent.thomas.md",
    siteName: "Open Agent",
    title: "Open Agent",
    description:
      "Self-hosted cloud orchestrator for AI coding agents (Claude Code & OpenCode). Mission orchestration, workspace management, and library sync.",
    images: [
      {
        url: "/og-image.png",
        width: 1200,
        height: 630,
        alt: "Open Agent - Cloud Orchestrator for AI Coding Agents",
      },
    ],
  },
  icons: {
    icon: "/favicon.svg",
    apple: "/apple-touch-icon.png",
  },
  appleWebApp: {
    capable: true,
    statusBarStyle: "black-translucent",
    title: "Open Agent",
  },
  other: {
    "msapplication-TileColor": "#0c0b0a",
  },
};

export default function RootLayout({
  children,
}: {
  children: ReactNode;
}) {
  return (
    <html lang="en" dir="ltr" suppressHydrationWarning>
      <Head>
        <meta
          name="theme-color"
          media="(prefers-color-scheme: light)"
          content="#ffffff"
        />
        <meta
          name="theme-color"
          media="(prefers-color-scheme: dark)"
          content="#0c0b0a"
        />
      </Head>
      <body className="min-h-dvh bg-mesh-subtle">
        <ThemeProvider>{children}</ThemeProvider>
      </body>
    </html>
  );
}
