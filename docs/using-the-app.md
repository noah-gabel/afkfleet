# Using the afkfleet app

afkfleet's desktop app lets you start, stop and watch the Minecraft bots your server's admin shares with you. It runs on 64-bit Windows 10 and 11.

## Installing the app
1. Download `afkfleet_X.Y.Z_x64-setup.exe` from the latest release: https://github.com/noah-gabel/afkfleet/releases/latest.
2. Optional: to check the download is complete, run `Get-FileHash "$HOME\Downloads\afkfleet_X.Y.Z_x64-setup.exe"` in PowerShell. The SHA-256 it prints must equal that file's line in the release's `SHA256SUMS` (capital or small letters don't matter).
3. Run it. The installer isn't signed with a paid code-signing certificate, so the first time Windows shows **"Windows protected your PC"**. Click **More info**, check that the app reads `afkfleet_X.Y.Z_x64-setup.exe` and the publisher reads "Unknown publisher", then click **Run anyway**. Only do this for an installer you got from that releases page.
4. afkfleet installs for your Windows user only and doesn't ask for administrator rights. If your PC doesn't have Microsoft's WebView2 runtime (Windows 11 has it built in), the installer downloads it first.
5. Success: afkfleet starts and asks for a server (see "Connecting to a server").

## Connecting to a server
You need two things from the server's admin: its address, such as `https://fleet.example.com`, and an invite code.

Start afkfleet. The first time, it opens **Server setup**. Enter the address as **Server address** and choose **Save**. Leave **Use a custom CA** unticked: the server's certificate comes from a public certificate authority such as Let's Encrypt, which Windows already trusts.

**A server without a public domain:** the admin sends you their HTTPS CA certificate as a file, and tells you its fingerprint another way (in person, or in a different chat than the file). In **Server setup**, tick **Use a custom CA**, load the file with **Choose file…**, and choose **Save**. Before you log in, choose **Server settings** on the login screen: under **Current custom CA**, compare each fingerprint with what they told you, character by character. If they differ, don't log in. Choose the right file or untick **Use a custom CA**, and ask the admin.

## Registering with an invite
1. On the login screen, choose **Register with an invite**, then enter the **Invite code**, a **Username**, a **Password** and **Repeat password**, and choose **Register**.
2. An invite works once and expires. If it's refused, ask your admin for a new one.

## Setting up 2FA
Every account uses two-factor authentication. Besides your password, you enter a 6-digit code from an authenticator app on your phone (any app for time-based codes).
1. **Set up 2FA:** scan the QR code with an authenticator app and enter its code.
2. Keep the **Recovery codes** somewhere safe, such as a password manager. Each one works once in place of an app code.
3. Lost your phone? Log in with a recovery code (choose **Use a recovery code** where the app asks for its code), then choose **Profile › Replace authenticator**.

## Logging in
Log in with your username, your password and a code from the app. Success: Home opens and shows the bots shared with you.

## Updates
Updates come through the app. At each start it checks GitHub, and when a newer version exists, a row at the top says "afkfleet X.Y.Z is available." Click **Install and restart**: a small installer window shows its progress, and afkfleet starts again by itself. **Check for updates** in **Profile › Updates** checks whenever you want. The app installs only updates signed with afkfleet's key. They're downloaded by the app, not a browser, so Windows normally doesn't warn again.

## Removing the app
Log out first, which removes the session this PC keeps in Windows' Credential Manager. Then uninstall it in Windows Settings:
- Windows 11: Apps › Installed apps, then the ⋯ next to afkfleet › Uninstall.
- Windows 10: Apps › Apps & features, then afkfleet › Uninstall.

Tick "Delete the application data" to remove the saved server address too.
