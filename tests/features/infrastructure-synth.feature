Feature: Infrastructure definition synthesizes the full environment set
  Asserts SC7. The footprint is exactly one shared stack plus three
  single-instance environment stacks — the fixed shape behind the
  fixed, predictable monthly cost under the forty-dollar ceiling.

  # Synth is the system boundary of the infrastructure app: the stack
  # listing is the observable promise of what a deploy would create.
  Scenario: cdk-synth-emits-four-stacks
    Given the maintainer has installed the infrastructure app dependencies
    When the maintainer synthesizes the infrastructure app
    Then the stack listing shows exactly four stacks: RelayShared, Relay-dev, Relay-staging, and Relay-prod
    And each environment stack defines exactly one relay instance
