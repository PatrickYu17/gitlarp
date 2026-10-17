import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = { title: "gitlarp" };

// Kumo reads light/dark from `data-mode` and renders light without it.
// Set it on <html> before first paint so nothing flashes, and because
// Kumo portals popups (Popover, Tooltip, Toast) to <body>, outside any
// page-level wrapper. /widget uses the same `?theme=` whitelist as its
// page (only "dark" is dark); every other route follows the OS.
// Inline is allowed by the CSP's script-src 'unsafe-inline' (middleware.ts).
const colorMode = `(function(){try{var d=document.documentElement,q=matchMedia("(prefers-color-scheme: dark)");if(/^\\/widget\\/?$/.test(location.pathname)){d.dataset.mode=new URLSearchParams(location.search).get("theme")==="dark"?"dark":"light";return}var f=function(){d.dataset.mode=q.matches?"dark":"light"};f();q.addEventListener("change",f)}catch(e){}})()`;

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    // suppressHydrationWarning: the script above rewrites data-mode
    // before React hydrates
    <html lang="en" data-mode="light" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: colorMode }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
