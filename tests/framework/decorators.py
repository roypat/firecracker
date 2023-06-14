# Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Module for declaring decorators used throughout integration tests."""

import re

from framework.defs import MAX_API_CALL_DURATION_MS


def timed_request(method):
    """Decorate functions to monitor their duration."""

    class ApiTimeoutException(Exception):
        """A custom exception containing the details of the failed API call."""

        def __init__(self, duration, method, resource, payload):
            """Compose the error message from the API call components."""
            super().__init__(
                f"API call exceeded maximum duration: {float(duration)} ms.\n"
                f"Call: {method} {resource} {payload}"
            )

    def timed(*args, **kwargs):
        """Raise an exception if method's duration exceeds the max value."""
        response = method(*args, **kwargs)
        timing_header = response.headers["Server-Timing"]

        FC_METRIC_REGEX = re.compile("fc;dur=(?P<duration_ms>\d+.\d{2})")

        match = FC_METRIC_REGEX.match(timing_header)

        duration_ms = float(match.group("duration_ms"))

        if duration_ms > MAX_API_CALL_DURATION_MS:
            try:
                # The positional arguments are:
                # 1. The Session object
                # 2. The URL from which we extract the resource for readability
                resource = args[1][(args[1].rfind("/")) :]
            except IndexError:
                # Ignore formatting errors.
                resource = ""

            # The payload is JSON-encoded and passed as an argument.
            payload = kwargs["json"] if "json" in kwargs else ""

            raise ApiTimeoutException(
                duration_ms, method.__name__.upper(), resource, payload
            )

        return response

    return timed
