Feature: Graded promotion gates on the deploy workflow
  Asserts SC5 and SC1. Dev deploys on demand with no approval pause,
  staging visibly pauses for a named human, and every deploy proves
  the environment answers at the published address before reporting
  success.

  # Dev is the ungated lane: a dispatch runs straight through to a
  # healthy endpoint.
  Scenario: dev-deploys-without-gate
    Given the dev environment carries no required reviewer
    When the maintainer dispatches the deploy workflow at the dev environment
    Then the workflow run proceeds to the deploy job with no approval pause
    And the WebSocket-upgrade probe against the published dev address returns HTTP 101 within the retry budget

  # Staging is the reviewer-gated lane: the pause is observable on the
  # workflow run itself.
  Scenario: staging-waits-for-review
    Given the staging environment names a required reviewer
    When the maintainer dispatches the deploy workflow at the staging environment
    Then the workflow run pauses visibly in the "Waiting for review" state
    And the deploy job proceeds only after the release manager approves the run
    And the WebSocket-upgrade probe against the published staging address returns HTTP 101
