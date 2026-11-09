# Google Drive example

Serves a Google Doc as JSON parsed from ArchieML. This is a port of archieml-js's
[`examples/google_drive.js`](https://github.com/newsdev/archieml-js/blob/master/examples/google_drive.js)
to [axum](https://docs.rs/axum) and [google-drive3](https://docs.rs/google-drive3).

Visiting `http://127.0.0.1:3000/{document-id}` signs you in with Google, exports the
document as HTML and converts it to ArchieML text, keeping links and list bullets
(a plain-text export loses links). It then responds with the parsed document. `/`
serves archieml-js's example document.

## Setup

1. In the [Google Cloud console](https://console.cloud.google.com/), enable the
   **Google Drive API** for a project.
2. Create an **OAuth client ID** of type *Web application*, with
   `http://127.0.0.1:3000/oauth2callback` as an authorized redirect URI.
3. Run the server with the client's credentials:

   ```sh
   GOOGLE_CLIENT_ID=... GOOGLE_CLIENT_SECRET=... cargo run -p google-drive-example
   ```

4. Open `http://127.0.0.1:3000/` or `http://127.0.0.1:3000/{document-id}`, where the
   document ID is the long part of the document's URL:
   `https://docs.google.com/document/d/{document-id}/edit`.

## Differences from the JavaScript example

- Credentials come from environment variables instead of the source.
- The document ID travels through the OAuth `state` parameter (also checked against
  CSRF, with PKCE) instead of a global, so concurrent sign-ins don't interfere.
- Only read access to Drive is requested.
- It uses version 3 of the Drive API, whose `files.export` replaces v2's export links.

The HTML-to-ArchieML conversion follows the JavaScript example's rules exactly.
