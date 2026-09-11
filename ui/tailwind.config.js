/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      colors: {
        naslos: {
          primary: '#2563eb',
          secondary: '#1e40af',
          accent: '#3b82f6',
          dark: '#0f172a',
          surface: '#1e293b',
          border: '#334155'
        }
      }
    }
  },
  plugins: []
};
