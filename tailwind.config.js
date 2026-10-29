/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        primary: {
          DEFAULT: '#6366f1',
          dark: '#4f46e5',
          light: '#818cf8',
        },
        surface: {
          DEFAULT: '#1e1e2e',
          light: '#2a2a3e',
          lighter: '#363650',
          dark: '#161622',
        },
        accent: {
          green: '#22c55e',
          red: '#ef4444',
          yellow: '#eab308',
        },
      },
    },
  },
  plugins: [],
};
