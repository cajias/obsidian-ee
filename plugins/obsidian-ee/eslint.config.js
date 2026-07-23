// ESLint Flat Config for Obsidian E2E Plugin
// Based on @lint-configs/eslint-config from cajias/lint-configs v1.0.6

const typescriptEslint = require('@typescript-eslint/eslint-plugin');
const typescriptParser = require('@typescript-eslint/parser');
const importPlugin = require('eslint-plugin-import');
const securityPlugin = require('eslint-plugin-security');
const sonarjsPlugin = require('eslint-plugin-sonarjs');
// v62+ uses ESM with default export in CJS wrapper (fix from lint-configs v1.0.6)
const unicornPlugin =
    require('eslint-plugin-unicorn').default || require('eslint-plugin-unicorn');
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
                document: 'readonly',
                HTMLElement: 'readonly',
            },
        },

        plugins: {
            '@typescript-eslint': typescriptEslint,
            import: importPlugin,
            security: securityPlugin,
            sonarjs: sonarjsPlugin,
            unicorn: unicornPlugin,
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

            // Unicorn best practices (v62+ compatible rules from lint-configs)
            'unicorn/better-regex': 'error',
            'unicorn/catch-error-name': 'error',
            'unicorn/consistent-destructuring': 'error',
            'unicorn/error-message': 'error',
            'unicorn/escape-case': 'error',
            'unicorn/new-for-builtins': 'error',
            'unicorn/no-abusive-eslint-disable': 'error',
            'unicorn/no-array-push-push': 'error',
            'unicorn/no-console-spaces': 'error',
            'unicorn/no-empty-file': 'error',
            'unicorn/no-instanceof-array': 'error',
            'unicorn/no-lonely-if': 'error',
            'unicorn/no-nested-ternary': 'error',
            'unicorn/no-new-array': 'error',
            'unicorn/no-new-buffer': 'error',
            'unicorn/no-useless-fallback-in-spread': 'error',
            'unicorn/no-useless-length-check': 'error',
            'unicorn/no-useless-promise-resolve-reject': 'error',
            'unicorn/no-useless-spread': 'error',
            'unicorn/no-useless-switch-case': 'error',
            'unicorn/no-zero-fractions': 'error',
            'unicorn/number-literal-case': 'error',
            'unicorn/prefer-array-find': 'error',
            'unicorn/prefer-array-flat': 'error',
            'unicorn/prefer-array-flat-map': 'error',
            'unicorn/prefer-array-index-of': 'error',
            'unicorn/prefer-array-some': 'error',
            'unicorn/prefer-at': 'error',
            'unicorn/prefer-date-now': 'error',
            'unicorn/prefer-default-parameters': 'error',
            'unicorn/prefer-includes': 'error',
            'unicorn/prefer-negative-index': 'error',
            'unicorn/prefer-number-properties': 'error',
            'unicorn/prefer-optional-catch-binding': 'error',
            'unicorn/prefer-spread': 'error',
            'unicorn/prefer-string-replace-all': 'error',
            'unicorn/prefer-string-slice': 'error',
            'unicorn/prefer-string-starts-ends-with': 'error',
            'unicorn/prefer-string-trim-start-end': 'error',
            'unicorn/prefer-type-error': 'error',
            'unicorn/throw-new-error': 'error',

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
            'unicorn/no-null': 'off',
            'security/detect-object-injection': 'off',
            'no-only-tests/no-only-tests': 'error',
        },
    },

    // Config files
    {
        files: ['*.config.js', '*.config.mjs'],
        rules: {
            '@typescript-eslint/no-var-requires': 'off',
        },
    },

    // Prettier integration
    prettierConfig,
];
