// ESLint flat config targeting the smells AI-authored JS/TS commits actually
// introduce. Merge the `rules` blocks into the project's eslint.config.mjs.
//
// Empirical ranking of AI-introduced JS/TS issues, most frequent first:
//   1. unused variables and parameters -> no-unused-vars
//   2. shadowed outer variable         -> no-shadow
//   3. block-scoped variable misuse    -> no-var, block-scoped-var, prefer-const
//   4. sequence expressions            -> no-sequences
//   5. path traversal via path.join    -> handled by review, not lint
//
// Introduce at the level the codebase currently passes, then ratchet. A rule set
// the team disables next week is worth less than five rules that stay on.
//
// TypeScript support is optional and self-guarding. Referencing an
// `@typescript-eslint/*` rule without the plugin installed makes ESLint abort the
// entire run ("could not find plugin"), so the typed rules are only added when
// `typescript-eslint` actually resolves. Install it to activate them:
//
//     npm i -D typescript-eslint
//
// The type-aware rules additionally need `projectService` (or `project`) wired
// up; without type information they are skipped, which is noted below.

let tseslint = null;
try {
  tseslint = (await import('typescript-eslint')).default;
} catch {
  // Not installed. JS linting still works; the typed block is omitted.
}

export default [
  {
    files: ['**/*.{js,mjs,cjs,jsx,ts,mts,cts,tsx}'],
    languageOptions: {
      ecmaVersion: 'latest',
      sourceType: 'module',
    },
    rules: {
      // --- the top four, non-negotiable ---
      'no-unused-vars': ['error', {
        args: 'after-used',
        argsIgnorePattern: '^_',
        varsIgnorePattern: '^_',
        caughtErrors: 'all',
        caughtErrorsIgnorePattern: '^_',
        ignoreRestSiblings: true,
      }],
      'no-shadow': 'error',
      'no-var': 'error',
      'block-scoped-var': 'error',
      'prefer-const': 'error',
      'no-sequences': 'error',

      // --- swallowed errors: the defect this codebase was cleaned up to remove ---
      // An empty block is almost always a discarded error.
      'no-empty': ['error', { allowEmptyCatch: false }],
      // `catch (e) {}` and `.catch(() => {})` must not appear. There is no lint
      // rule that catches the promise form, so CI also runs
      // `slopfix smells --severity blocking --strict`, which does.
      'no-useless-catch': 'error',
      'prefer-promise-reject-errors': 'error',
      'no-throw-literal': 'error',

      // --- correctness ---
      'eqeqeq': ['error', 'always', { null: 'ignore' }],
      'no-implicit-coercion': 'warn',
      'no-param-reassign': ['error', { props: false }],
      'no-return-assign': 'error',
      'no-fallthrough': 'error',
      'array-callback-return': 'error',
      'require-atomic-updates': 'error',
      'no-await-in-loop': 'warn',
      'no-constant-binary-expression': 'error',
      'no-self-compare': 'error',
      'no-unmodified-loop-condition': 'error',
      'no-unreachable-loop': 'error',
      'no-promise-executor-return': 'error',
      'no-unsafe-optional-chaining': 'error',

      // --- duplication and dead weight ---
      'no-dupe-else-if': 'error',
      'no-duplicate-case': 'error',
      'no-duplicate-imports': 'error',
      'no-lonely-if': 'warn',
      'no-useless-rename': 'error',
      'no-useless-return': 'error',
      'no-useless-concat': 'error',
      'no-else-return': ['warn', { allowElseIf: false }],

      // --- maintainability: god-function thresholds ---
      // Start where the codebase sits and lower over time.
      'complexity': ['warn', 15],
      'max-depth': ['warn', 4],
      'max-params': ['warn', 5],
      'max-lines-per-function': ['warn', {
        max: 80,
        skipBlankLines: true,
        skipComments: true,
      }],

      // --- leftovers ---
      'no-console': ['warn', { allow: ['warn', 'error'] }],
      'no-debugger': 'error',
      'no-alert': 'error',
    },
  },

  // TypeScript: the typed rules replace their base counterparts. Included only
  // when typescript-eslint resolves, because naming a rule from a missing plugin
  // aborts the whole lint run rather than degrading.
  ...(tseslint
    ? [
        {
          files: ['**/*.{ts,mts,cts,tsx}'],
          plugins: { '@typescript-eslint': tseslint.plugin },
          languageOptions: { parser: tseslint.parser },
          rules: {
            'no-unused-vars': 'off',
            'no-shadow': 'off',
            '@typescript-eslint/no-unused-vars': ['error', {
              args: 'after-used',
              argsIgnorePattern: '^_',
              varsIgnorePattern: '^_',
              caughtErrors: 'all',
              caughtErrorsIgnorePattern: '^_',
            }],
            '@typescript-eslint/no-shadow': 'error',
            // `any` is how type errors get silenced instead of fixed.
            '@typescript-eslint/no-explicit-any': 'warn',
          },
        },
        // Type-aware rules. These need type information, so they are scoped
        // separately and require `projectService` to be enabled. Uncomment once
        // the project has a tsconfig ESLint can resolve.
        // {
        //   files: ['**/*.{ts,mts,cts,tsx}'],
        //   plugins: { '@typescript-eslint': tseslint.plugin },
        //   languageOptions: {
        //     parser: tseslint.parser,
        //     parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
        //   },
        //   rules: {
        //     '@typescript-eslint/no-unnecessary-condition': 'warn',
        //     '@typescript-eslint/no-floating-promises': 'error',
        //     '@typescript-eslint/await-thenable': 'error',
        //     '@typescript-eslint/no-misused-promises': 'error',
        //     // `catch (e: any)` re-opens the broad-catch hole.
        //     '@typescript-eslint/use-unknown-in-catch-callback-variable': 'error',
        //   },
        // },
      ]
    : []),

  // Tests legitimately reach into internals and use long setup functions.
  {
    files: ['**/*.{test,spec}.{js,jsx,ts,tsx}', '**/__tests__/**', 'tests/**'],
    rules: {
      'max-lines-per-function': 'off',
      'no-console': 'off',
      '@typescript-eslint/no-explicit-any': 'off',
    },
  },
];
