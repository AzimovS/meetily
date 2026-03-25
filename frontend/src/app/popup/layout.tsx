// Minimal layout for popup windows — no sidebar, no providers, no chrome
export default function PopupLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body
        style={{
          margin: 0,
          padding: 0,
          width: '100vw',
          height: '100vh',
          overflow: 'hidden',
          background: 'transparent',
        }}
      >
        {children}
      </body>
    </html>
  );
}
