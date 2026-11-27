/** @type {import('tailwindcss').Config} */
// Theme strategy: every color below resolves to a CSS variable holding an
// "R G B" triplet, so Tailwind keeps generating the same class names
// (`bg-green-900/50`, `text-gray-400`, …) while the values flip with
// `data-theme` on <html>. The <alpha-value> placeholder keeps every opacity
// modifier working. Dark-theme values live in ui/src/app.css :root and are the
// exact pre-overhaul palette; light-theme values are defined under
// [data-theme='light'] there.
const rgb = (name) => `rgb(var(${name}) / <alpha-value>)`;

// Only shades actually used by the UI are mapped. If a new shade is introduced,
// add the variable to both themes in app.css and list it here; an unmapped
// shade would silently keep a fixed color and stop flipping with the theme.
// Text shades (100-500) get ink-grade light values; 900/950 are badge/tint
// backgrounds and get light tints; solid button shades stay saturated.
const shaded = (prefix, shades) =>
  Object.fromEntries(shades.map((s) => [s, rgb(`--${prefix}-${s}`)]));

export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      colors: {
        naslos: {
          primary: rgb('--n-primary'),
          'primary-hover': rgb('--n-primary-hover'),
          secondary: rgb('--n-secondary'),
          accent: rgb('--n-accent'),
          dark: rgb('--n-dark'),
          surface: rgb('--n-surface'),
          'surface-raised': rgb('--n-surface-raised'),
          border: rgb('--n-border'),
          'border-strong': rgb('--n-border-strong'),
          // The strongest text color on a theme surface: white in dark,
          // near-black ink in light. Use where `text-white` would sit on a
          // surface that flips (modal close buttons, copy buttons, emphasis).
          strong: rgb('--n-strong'),
          // Link/accent ink for text on theme surfaces. The brand primary
          // (#2563eb) fails AA as dark-theme text, so text usage goes through
          // this token: blue-400 in dark (5.4:1 on the card surface),
          // blue-700 in light (6.3:1 on white).
          link: rgb('--n-link')
        },
        gray: shaded('gray', ['100', '200', '300', '400', '500', '600', '800', '900']),
        green: shaded('green', ['200', '300', '400', '500', '700', '800', '900']),
        red: shaded('red', ['200', '300', '400', '500', '600', '700', '800', '900', '950']),
        yellow: shaded('yellow', ['300', '400', '500', '700', '900']),
        amber: shaded('amber', ['100', '200', '300', '400', '700', '800', '900', '950']),
        blue: shaded('blue', ['300', '400', '900']),
        purple: shaded('purple', ['300', '400', '900']),
        emerald: shaded('emerald', ['200', '300', '700', '900'])
      },
      fontFamily: {
        sans: ['"Fira Sans"', 'ui-sans-serif', 'system-ui', 'sans-serif'],
        mono: ['"Fira Code"', 'ui-monospace', 'SFMono-Regular', 'Menlo', 'monospace']
      }
    }
  },
  plugins: []
};
