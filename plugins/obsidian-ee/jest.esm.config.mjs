/** @type {import('ts-jest').JestConfigWithTsJest} */
export default {
  preset: 'ts-jest/presets/default-esm',
  // A failing BigInt assertion (e.g. `expect(epochOf(client)).toBe(2n)`)
  // crashes the worker's IPC report with "Do not know how to serialize a
  // BigInt" (suite shows 0 tests, real diff never prints) unless
  // BigInt.prototype.toJSON exists in the worker's own realm -- see the
  // comment in jest.node-environment.cjs for why this can't be a plain
  // setupFilesAfterEnv file.
  testEnvironment: '<rootDir>/jest.node-environment.cjs',
  roots: ['<rootDir>/src'],
  testMatch: ['**/__tests__/**/*.test.ts', '**/*.test.ts'],
  extensionsToTreatAsEsm: ['.ts'],
  transform: {
    // Type-check test files during the run (ts-jest default). @jest/globals
    // types jest.fn() precisely (Mock<UnknownFunction>), so mock signatures are
    // enforced here just as `tsc --noEmit` enforces them.
    '^.+\\.tsx?$': ['ts-jest', { useESM: true, tsconfig: 'tsconfig.json' }],
  },
  moduleNameMapper: {
    '^@/(.*)$': '<rootDir>/src/$1',
    '^(\\.{1,2}/.*)\\.js$': '$1',
  },
  collectCoverageFrom: [
    'src/**/*.ts',
    '!src/**/*.test.ts',
    '!src/**/__tests__/**',
    '!src/**/__mocks__/**',
  ],
  coverageDirectory: 'coverage',
  coverageReporters: ['text', 'lcov', 'html'],
  verbose: true,
};
