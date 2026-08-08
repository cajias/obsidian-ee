Feature: Relay container shuts down gracefully on stop
  Asserts SC6. A deploy or rollback is stop-then-start on a singleton,
  so the container must exit promptly on the stop signal for recovery
  to stay inside the fifteen-minute budget.

  # The image declares SIGINT as the stop signal; the relay handles only
  # SIGINT, so a container stop lands as a signal the process catches.
  Scenario: relay-container-stops-on-sigint
    Given the maintainer has built the relay image from the repository Dockerfile
    And a relay container is running from that image
    When the maintainer stops the container
    Then the relay process receives the declared stop signal and exits promptly
    And the container reaches the stopped state within the stop-grace period, never by a kill at the timeout
