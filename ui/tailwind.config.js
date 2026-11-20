/** @type {import('tailwindcss').Config} */
// Theme strategy: every color below resolves to a CSS variable holding an
// "R G B" triplet, so Tailwind keeps generating the same class names
// (`bg-green-900/50`, `text-gray-400`, …) while the values flip with
// `data-theme` on <html>. The <alpha-value> placeholder keeps every opacity
// modifier working. Dark-theme values live in ui/src/app.css :root and are the
// exact pre-overhaul palette; light-theme values are defined under
// [data-theme='light'] there.
const rgb = (name) => `rgb(var(${name}) / <alpha-value>)`;

// Shades actually used by the codebase, per family, mapped to vars. Unused
// shades keep Tailwind defaults. Text shades (200-500) get ink-grade light
// values; the 900/950 shades are badge/tint backgrounds and get light tints.
const shaded = (prefix, shades) =>
  Object.fromEntries(shades.map((s) => [s, rgb(`--${prefix}-${s}`)]));

export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      colors: {
        naslos: {
          primary: rgb('--n-primary'),
          secondary: rgb('--n-secondary'),
          accent: rgb('--n-accent'),
          dark: rgb('--n-dark'),
          surface: rgb('--n-surface'),
          border: rgb('--n-border'),
          // The strongest text color on a theme surface: white in dark,
          // near-black ink in light. Use where `text-white` would sit on a
          // surface that flips (modal close buttons, copy buttons, emphasis).
          strong: rgb('--n-strong')
        },
        gray: shaded('gray', ['100', '200', '300', '400', '500', '600', '800', '900']),
        green: shaded('green', ['200', '300', '400', '900']),
        red: shaded('red', ['200', '300', '400', '900', '950']),
        yellow: shaded('yellow', ['300', '400', '500', '900']),
        amber: shaded('amber', ['200', '300', '400', '900', '950']),
        blue: shaded('blue', ['300', '400', '900']),
        purple: shaded('purple', ['300', '400', '900']),
        emerald: shaded('emerald', ['200', '300', '900'])
      }
    }
  },
  plugins: []
};
