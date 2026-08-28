Feature: Verified release rollout, admission control, and rollback
  Asserts SC3, SC5, SC1, SC2, and SC6. Production runs only cut
  releases promoted byte-identical, every environment serves
  credentialed collaborators and refuses everyone else, and a prior
  release restores from the existing digest with no rebuild.

  # The release cut is the prod gate; same-digest promotion is
  # observable in the registry.
  Scenario: prod-deploys-on-release-cut
    Given a fix commit merged to main has produced a release PR
    When the release manager merges the release PR and release v0.1.1 publishes
    Then the deploy workflow fires at the prod environment with the released version
    And the registry shows the v0.1.1 tag and the commit tag naming one and the same image digest

  # The handshake completes before authentication, so an unauthenticated
  # upgrade is a valid liveness probe of every environment.
  Scenario: handshake-returns-101
    Given the maintainer has deployed all three environments
    When the maintainer sends a WebSocket-upgrade request to each published address
    Then every environment answers the upgrade with HTTP status 101

  # Admission happens in the Identify exchange after the handshake; the
  # refusal proves no environment operates as an open relay.
  Scenario: unauthenticated-identify-rejected
    Given the collaborator connects to an environment's published address
    When the collaborator sends an Identify message carrying no valid credential
    Then the relay answers with a rejection and admits no session
    And the collaborator presenting the environment's valid credential in Identify receives an ack and relays a session end to end

  # Rollback is the same machinery pointed at an existing release tag.
  Scenario: rollback-redeploys-old-digest
    Given the registry already holds the digest named by the prior release tag v0.1.0
    When the maintainer dispatches the deploy workflow at prod from the tag v0.1.0
    Then the prod environment runs the digest the v0.1.0 tag already named
    And the workflow run performs no image rebuild
