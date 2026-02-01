import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import "./globals.css";
import { Sidebar } from "@/components/sidebar";
import { AuthGate } from "@/components/auth-gate";
import { LibraryProvider } from "@/contexts/library-context";
import { MissionSwitcherProvider } from "@/contexts/mission-switcher-context";
import { ToastProvider } from "@/components/toast";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin"],
});

export const metadata: Metadata = {
  title: "Sandboxed.sh",
  description: "Autonomous coding agents in isolated environments",
  icons: {
    icon: "/favicon.svg",
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en" className="dark">
      <body
        className={`${geistSans.variable} ${geistMono.variable} antialiased`}
      >
        <AuthGate>
          <ToastProvider>
            <LibraryProvider>
              <MissionSwitcherProvider>
                <Sidebar />
                <main className="ml-56 min-h-screen">{children}</main>
              </MissionSwitcherProvider>
            </LibraryProvider>
          </ToastProvider>
        </AuthGate>
      </body>
    </html>
  );
}
