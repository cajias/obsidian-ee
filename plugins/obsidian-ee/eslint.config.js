// ESLint Flat Config for Obsidian E2E Plugin
// Based on @lint-configs/eslint-config from cajias/lint-configs

const typescriptEslint = require('@typescript-eslint/eslint-plugin');
const typescriptParser = require('@typescript-eslint/parser');
const importPlugin = require('eslint-plugin-import');
const securityPlugin = require('eslint-plugin-security');
const sonarjsPlugin = require('eslint-plugin-sonarjs');
// Note: unicorn plugin removed due to breaking changes in v62+
const promisePlugin = require('eslint-plugin-promise');
const prettierConfig = require('eslint-config-prettier');
const prettierPlugin = require('eslint-plugin-prettier');
const noOnlyTestsPlugin = require('eslint-plugin-no-only-tests');

module.exports = [
  {
    files: ['src/**/*.ts'],
    ignores: ['src/wasm/**/*', 'node_modules/**/*', 'main.js'],

    languageOptions: {
      parser: typescriptParser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
        project: './tsconfig.json',
      },
      globals: {
        console: 'readonly',
        setTimeout: 'readonly',
        clearTimeout: 'readonly',
        NodeJS: 'readonly',
        WebSocket: 'readonly',
      },
    },

    plugins: {
      '@typescript-eslint': typescriptEslint,
      import: importPlugin,
      security: securityPlugin,
      sonarjs: sonarjsPlugin,
      promise: promisePlugin,
      prettier: prettierPlugin,
      'no-only-tests': noOnlyTestsPlugin,
    },

    rules: {
      // Security
      'security/detect-object-injection': 'warn',
      'security/detect-non-literal-regexp': 'error',
      'security/detect-unsafe-regex': 'error',

      // TypeScript strict checks
      '@typescript-eslint/no-explicit-any': 'warn',
      '@typescript-eslint/no-unused-vars': [
        'error',
        {
          argsIgnorePattern: '^_',
          varsIgnorePattern: '^_',
          caughtErrorsIgnorePattern: '^_',
        },
      ],
      '@typescript-eslint/no-floating-promises': 'error',
      '@typescript-eslint/await-thenable': 'error',

      // Code quality
      eqeqeq: ['error', 'always'],
      'no-var': 'error',
      'prefer-const': 'error',
      'no-eval': 'error',

      // Complexity (relaxed for MVP)
      complexity: ['warn', 15],
      'max-depth': ['warn', 5],
      'max-lines-per-function': ['warn', { max: 75, skipBlankLines: true, skipComments: true }],

      // Error handling
      'no-throw-literal': 'error',
      'promise/catch-or-return': 'error',
      'promise/no-return-wrap': 'error',

      // Common issues
      'no-console': 'off', // Allow console for plugin debugging
      'no-debugger': 'error',

      // SonarJS
      'sonarjs/no-duplicate-string': ['warn', { threshold: 4 }],
      'sonarjs/no-identical-expressions': 'error',
      'sonarjs/cognitive-complexity': ['warn', 20],

      // Prettier
      'prettier/prettier': 'error',
    },
  },

  // Test files - relaxed rules
  {
    files: ['src/__tests__/**/*.ts', '**/*.test.ts', '**/*.spec.ts'],
    rules: {
      '@typescript-eslint/no-explicit-any': 'off',
      'max-lines-per-function': 'off',
      'sonarjs/no-duplicate-string': 'off',
      'sonarjs/cognitive-complexity': 'off',
      'no-only-tests/no-only-tests': 'error',
    },
  },

  // Config files
  {
    files: ['*.config.js', '*.config.mjs', 'jest.config.js'],
    rules: {
      '@typescript-eslint/no-var-requires': 'off',
    },
  },

  // Prettier integration
  prettierConfig,
];
