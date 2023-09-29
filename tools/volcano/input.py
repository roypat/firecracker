#!/usr/bin/env python3
# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0

"""Module dealing with presenting the user with dimension selections"""
import inquirer
import scipy


def eliminate_unchosen(df, answer):
    """Based on the given inquirer answer, eliminate the deselected rows from the given dataframe"""
    dimension, values = list(answer.items())[0]
    reduced = df[df[dimension].isin(values) | df[dimension].isna()]

    return reduced


def entropy(df, dimension):
    """Determines the Shannon entropy of the given dimension in the provided dataframe"""
    if not set(df[dimension].dropna()):
        return None
    return scipy.stats.entropy(df.groupby(dimension).apply(len))


def maximum_entropy_dimension(df, dimensions):
    """From the given list of dimensions, returns the one with the higher Shannon-entropy in the provided dataframe"""
    entropies = {dimension: entropy(df, dimension) for dimension in dimensions}
    # We have to differentiate between entropy(df, dimension) is None (which means that the dimension is not
    # applicable anymore given our selection so far, for example the 'fio_mode' dimension if we selected test_restore_latency
    # as our performance test), and entropy(df, dimension) == 0, in which case the dimension is applicable, but
    # already completely determined (for example the io_engine dimension is host_kernel == 4.14, which forces it to be
    # Sync).
    entropies = {
        dimension: ent for dimension, ent in entropies.items() if ent is not None
    }

    if not entropies:
        return None

    return max(entropies, key=lambda dimension: entropies[dimension])


def inquire_dimension(df, dimension, previous_choices):
    choices = list(set(df[dimension].dropna()))

    if len(choices) == 1:
        print(
            f"Value of dimension '{dimension}' is pre-determined to be '{choices[0]}' by previous selections."
        )
        return {dimension: choices}

    # Lets see if any of the options conflict with previously selected dimensions
    non_forcing_choices = {
        dim: choices for dim, choices in previous_choices.items() if len(choices) > 1
    }

    if non_forcing_choices:
        potential_conflicts = (
            df[[*non_forcing_choices.keys(), dimension]]
            .groupby(list(non_forcing_choices))[dimension]
            .apply(set)
        )

        # TODO: Determine if any of the choices here retroactively eliminates some
        # previously selected dimensions. Need to build a decision tree and minimize it.

    message = f"Please pick from the below values for dimension '{dimension}'"

    while True:
        answer = inquirer.prompt(
            [inquirer.Checkbox(dimension, message=message, choices=sorted(choices))],
            raise_keyboard_interrupt=True,
        )

        if answer[dimension]:
            return answer


def prompt_for_buildkite_run(
    message="What's the build number (found in the run URL) of the run you want to display?",
):
    answer = inquirer.prompt(
        [
            inquirer.Text(
                "build_number",
                message=message,
            )
        ]
    )

    if not answer:
        return None
    return int(answer["build_number"])


def selection_in_loop(message: str, options, *, default=None):
    while True:
        answer = inquirer.prompt(
            [
                inquirer.List(
                    "action",
                    message=message,
                    choices=options,
                    default=default,
                )
            ]
        )

        if not answer:
            return

        yield answer["action"]
