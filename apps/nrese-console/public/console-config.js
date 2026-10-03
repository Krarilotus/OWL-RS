// Runtime configuration of the console, read before it starts. Unset values fall back to
// the build's (VITE_NRESE_API_BASE_URL), then to the console's own origin. To point a
// built console at another server without rebuilding, set
//   apiBaseUrl: "https://nrese.example.org"
// and `apiBaseUrl: ""` asks for the console's own origin even if the build named another.
window.__NRESE_CONSOLE_CONFIG__ = window.__NRESE_CONSOLE_CONFIG__ || {};
