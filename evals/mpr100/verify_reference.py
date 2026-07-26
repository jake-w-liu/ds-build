#!/usr/bin/env python3
"""Independent executable checks for the MPR-100 development reference."""

from __future__ import annotations

import itertools

import mpmath as mp
import sympy as sp


PASSED: list[str] = []


def verify(name: str, condition: object) -> None:
    if not bool(condition):
        raise AssertionError(name)
    PASSED.append(name)


def main() -> None:
    mp.mp.dps = 60

    # M01
    matrix_a = sp.Matrix([[2, 1, 0], [0, 3, 1], [1, 0, 2]])
    vector_u = sp.Matrix([1, -2, 3])
    vector_v = sp.Matrix([2, 1, 1])
    alpha = sp.Rational(5, 7)
    matrix_b = matrix_a + alpha * vector_u * vector_v.T
    sigma = 1 + alpha * (vector_v.T * matrix_a.inv() * vector_u)[0]
    verify("M01 determinant", matrix_b.det() == matrix_a.det() * sigma)
    inverse = (
        matrix_a.inv()
        - alpha
        * matrix_a.inv()
        * vector_u
        * vector_v.T
        * matrix_a.inv()
        / sigma
    )
    verify("M01 inverse", sp.simplify(matrix_b * inverse) == sp.eye(3))
    beta = (vector_v.T * matrix_a.inv() * vector_u)[0]
    singular_b = matrix_a - vector_u * vector_v.T / beta
    verify("M01 right null", singular_b * matrix_a.inv() * vector_u == sp.zeros(3, 1))
    verify(
        "M01 left null",
        (matrix_a.T.inv() * vector_v).T * singular_b == sp.zeros(1, 3),
    )
    verify("M01 singular rank", singular_b.rank() == 2)

    # M05
    t, k = sp.symbols("t k", positive=True)
    first = k * (sp.exp(-t) - sp.exp(-2 * t))
    verify("M05 ODE", sp.simplify(sp.diff(first, t) + first - k * sp.exp(-2 * t)) == 0)
    verify("M05 maximum time", sp.diff(first, t).subs(t, sp.log(2)) == 0)
    verify("M05 maximum value", sp.simplify(first.subs(t, sp.log(2)) - k / 4) == 0)

    # M12
    integral = mp.quad(lambda value: mp.log(1 + value**2) / (1 + value**2), [0, 1, mp.inf])
    verify("M12 integral", abs(integral - mp.pi * mp.log(2)) < mp.mpf("1e-50"))

    # M18
    x, epsilon = sp.symbols("x epsilon", positive=True)
    exact = (1 - sp.exp(-x / epsilon)) / (1 - sp.exp(-1 / epsilon))
    verify("M18 residual", sp.simplify(epsilon * sp.diff(exact, x, 2) + sp.diff(exact, x)) == 0)
    verify("M18 boundaries", exact.subs(x, 0) == 0 and sp.simplify(exact.subs(x, 1)) == 1)
    composite = 1 - sp.exp(-x / epsilon)
    expected_error = (
        (1 - sp.exp(-x / epsilon))
        * sp.exp(-1 / epsilon)
        / (1 - sp.exp(-1 / epsilon))
    )
    verify("M18 composite error", sp.simplify(exact - composite - expected_error) == 0)

    # M21
    for size in range(2, 9):
        for probability in (
            sp.Rational(1, 3),
            sp.Rational(2, 5),
            sp.Rational(3, 5),
            sp.Rational(3, 4),
        ):
            complement = 1 - probability
            ratio = complement / probability
            hit = [
                (1 - ratio**index) / (1 - ratio**size)
                for index in range(size + 1)
            ]
            duration = [
                (size * hit[index] - index) / (probability - complement)
                for index in range(size + 1)
            ]
            verify(
                f"M21 biased recurrence N={size} p={probability}",
                all(
                    sp.simplify(
                        hit[index]
                        - probability * hit[index + 1]
                        - complement * hit[index - 1]
                    )
                    == 0
                    and sp.simplify(
                        duration[index]
                        - 1
                        - probability * duration[index + 1]
                        - complement * duration[index - 1]
                    )
                    == 0
                    for index in range(1, size)
                ),
            )
        fair_hit = [sp.Rational(index, size) for index in range(size + 1)]
        fair_duration = [index * (size - index) for index in range(size + 1)]
        verify(
            f"M21 fair recurrence N={size}",
            all(
                fair_hit[index] == (fair_hit[index + 1] + fair_hit[index - 1]) / 2
                and fair_duration[index]
                == 1 + (fair_duration[index + 1] + fair_duration[index - 1]) / 2
                for index in range(1, size)
            ),
        )

    # M25
    transition = sp.Matrix(
        [
            [sp.Rational(2, 3), sp.Rational(1, 3), 0],
            [sp.Rational(1, 4), sp.Rational(11, 20), sp.Rational(1, 5)],
            [0, sp.Rational(1, 2), sp.Rational(1, 2)],
        ]
    )
    stationary = sp.Matrix([[sp.Rational(15, 43), sp.Rational(20, 43), sp.Rational(8, 43)]])
    verify("M25 stationary", stationary * transition == stationary)
    verify(
        "M25 detailed balance",
        all(
            stationary[0, row] * transition[row, column]
            == stationary[0, column] * transition[column, row]
            for row in range(3)
            for column in range(3)
        ),
    )
    verify(
        "M25 return times",
        [1 / stationary[0, index] for index in range(3)]
        == [sp.Rational(43, 15), sp.Rational(43, 20), sp.Rational(43, 8)],
    )

    # M31
    x = sp.symbols("x", real=True)
    riccati = x - sp.sqrt(2) * sp.tanh(sp.sqrt(2) * x)
    verify(
        "M31 residual",
        sp.simplify(
            sp.diff(riccati, x)
            - (riccati**2 - 2 * x * riccati + x**2 - 1)
        )
        == 0,
    )
    verify("M31 initial value", riccati.subs(x, 0) == 0)

    # M34
    gamma, omega_zero, t = sp.symbols("gamma omega_zero t", positive=True)
    omega_d = sp.sqrt(omega_zero**2 - gamma**2)
    green = sp.exp(-gamma * t) * sp.sin(omega_d * t) / omega_d
    verify(
        "M34 homogeneous residual",
        sp.simplify(
            sp.diff(green, t, 2)
            + 2 * gamma * sp.diff(green, t)
            + omega_zero**2 * green
        )
        == 0,
    )
    verify(
        "M34 jump data",
        green.subs(t, 0) == 0 and sp.simplify(sp.diff(green, t).subs(t, 0)) == 1,
    )

    # M41
    for size in range(10):
        brute_force = sum(
            1
            for values in itertools.product(range(3), repeat=size)
            if all(values.count(label) % 2 == 1 for label in range(3))
        )
        formula = ((3**size - 3) * (1 - (-1) ** size)) // 8
        verify(f"M41 brute force n={size}", brute_force == formula)

    # M43
    verify(
        "M43 CRT",
        [value for value in range(420) if value % 84 == 17 and value % 60 == 5]
        == [185],
    )

    # P51
    theta, omega, gravity_scale = sp.symbols("theta omega gravity_scale", positive=True)
    acceleration = sp.sin(theta) * (omega**2 * sp.cos(theta) - gravity_scale)
    verify(
        "P51 bottom curvature",
        sp.diff(acceleration, theta).subs(theta, 0) == omega**2 - gravity_scale,
    )
    verify(
        "P51 top curvature",
        sp.diff(acceleration, theta).subs(theta, sp.pi) == omega**2 + gravity_scale,
    )
    cosine = gravity_scale / omega**2
    off_axis = (
        sp.diff(acceleration, theta)
        .subs(sp.cos(theta), cosine)
        .subs(sp.sin(theta) ** 2, 1 - cosine**2)
    )
    verify(
        "P51 off-axis curvature",
        sp.simplify(off_axis + omega**2 * (1 - cosine**2)) == 0,
    )

    # P54
    mass, initial_mass, rate, drag, exhaust = sp.symbols(
        "mass initial_mass rate drag exhaust", positive=True
    )
    velocity = rate * exhaust / drag * (
        1 - (mass / initial_mass) ** (drag / rate)
    )
    verify(
        "P54 mass ODE",
        sp.simplify(
            -rate * mass * sp.diff(velocity, mass)
            - (rate * exhaust - drag * velocity)
        )
        == 0,
    )
    verify("P54 initial value", velocity.subs(mass, initial_mass) == 0)
    verify(
        "P54 zero-drag limit",
        sp.simplify(
            sp.limit(velocity, drag, 0) - exhaust * sp.log(initial_mass / mass)
        )
        == 0,
    )

    # P61
    z, radius, density, permittivity = sp.symbols(
        "z radius density permittivity", positive=True
    )
    potential = density / (2 * permittivity) * (
        sp.sqrt(z**2 + radius**2) - z
    )
    field = density / (2 * permittivity) * (
        1 - z / sp.sqrt(z**2 + radius**2)
    )
    verify("P61 field derivative", sp.simplify(field + sp.diff(potential, z)) == 0)
    verify(
        "P61 near plane",
        sp.limit(field, z, 0, dir="+") == density / (2 * permittivity),
    )
    verify(
        "P61 far potential",
        sp.limit(z * potential, z, sp.oo) == density * radius**2 / (4 * permittivity),
    )
    verify(
        "P61 far field",
        sp.limit(z**2 * field, z, sp.oo) == density * radius**2 / (4 * permittivity),
    )

    # P65
    frequency, light_speed, width = sp.symbols(
        "frequency light_speed width", positive=True
    )
    propagation = sp.sqrt(
        frequency**2 / light_speed**2 - (sp.pi / width) ** 2
    )
    phase_velocity = frequency / propagation
    group_velocity = 1 / sp.diff(propagation, frequency)
    verify(
        "P65 velocity product",
        sp.simplify(phase_velocity * group_velocity - light_speed**2) == 0,
    )

    # P71
    def even_function(value: mp.mpf, well: mp.mpf) -> mp.mpf:
        return value * mp.tan(value) - mp.sqrt(well**2 - value**2)

    def odd_function(value: mp.mpf, well: mp.mpf) -> mp.mpf:
        return -value / mp.tan(value) - mp.sqrt(well**2 - value**2)

    def bisect_root(
        function: object,
        lower: mp.mpf,
        upper: mp.mpf,
        *,
        iterations: int = 256,
    ) -> mp.mpf:
        callable_function = function
        lower_value = callable_function(lower)  # type: ignore[operator]
        upper_value = callable_function(upper)  # type: ignore[operator]
        if lower_value == 0:
            return lower
        if upper_value == 0:
            return upper
        if lower_value * upper_value >= 0:
            raise AssertionError("root bracket does not change sign")
        for _ in range(iterations):
            midpoint = (lower + upper) / 2
            midpoint_value = callable_function(midpoint)  # type: ignore[operator]
            if midpoint_value == 0:
                return midpoint
            if lower_value * midpoint_value < 0:
                upper = midpoint
            else:
                lower = midpoint
                lower_value = midpoint_value
        return (lower + upper) / 2

    shallow = mp.mpf("0.01")
    shallow_even = bisect_root(
        lambda value: even_function(value, shallow),
        mp.mpf("1e-12"),
        shallow - mp.mpf("1e-12"),
    )
    verify("P71 shallow even state", 0 < shallow_even < shallow)
    below = mp.mpf("1.55")
    above = mp.mpf("1.60")
    verify(
        "P71 no odd state below threshold",
        mp.pi / 2 > below,
    )
    odd_root = bisect_root(
        lambda value: odd_function(value, above),
        mp.pi / 2 + mp.mpf("1e-9"),
        above - mp.mpf("1e-9"),
    )
    verify("P71 odd state above threshold", mp.pi / 2 < odd_root < above)

    # P72
    detuning, coupling, t = sp.symbols("detuning coupling t", positive=True)
    rabi = sp.sqrt(detuning**2 + coupling**2)
    hamiltonian = sp.Matrix([[detuning, coupling], [coupling, -detuning]]) / 2
    evolution = (
        sp.eye(2) * sp.cos(rabi * t / 2)
        - sp.I * (2 * hamiltonian / rabi) * sp.sin(rabi * t / 2)
    )
    amplitude = evolution[0, 1]
    probability = coupling**2 / rabi**2 * sp.sin(rabi * t / 2) ** 2
    verify(
        "P72 symbolic transition",
        sp.simplify(sp.conjugate(amplitude) * amplitude - probability) == 0,
    )
    for detuning_value, coupling_value, time_value in (
        (mp.mpf("1.3"), mp.mpf("0.7"), mp.mpf("2.1")),
        (mp.mpf("-2.0"), mp.mpf("0.4"), mp.mpf("0.9")),
        (mp.mpf("0.0"), mp.mpf("3.0"), mp.pi / 3),
        (mp.mpf("1.0"), mp.mpf("0.0"), mp.mpf("4.2")),
    ):
        numerical_h = mp.matrix(
            [
                [detuning_value, coupling_value],
                [coupling_value, -detuning_value],
            ]
        ) / 2
        numerical_u = mp.expm(-1j * numerical_h * time_value)
        actual = abs(numerical_u[0, 1]) ** 2
        numerical_rabi = mp.sqrt(detuning_value**2 + coupling_value**2)
        expected = (
            coupling_value**2
            / numerical_rabi**2
            * mp.sin(numerical_rabi * time_value / 2) ** 2
        )
        verify(
            f"P72 numerical detuning={detuning_value}",
            abs(actual - expected) < mp.mpf("1e-50"),
        )

    # P81
    x = sp.symbols("x", positive=True)
    heat_shape = x**2 * sp.exp(x) / (1 + sp.exp(x)) ** 2
    logarithmic_derivative = sp.diff(sp.log(heat_shape), x)
    exponential_form = 2 / x + (1 - sp.exp(x)) / (1 + sp.exp(x))
    verify(
        "P81 logarithmic derivative",
        sp.simplify(logarithmic_derivative - exponential_form)
        == 0,
    )
    verify(
        "P81 hyperbolic identity",
        sp.simplify(
            sp.tanh(x / 2).rewrite(sp.exp)
            - (sp.exp(x) - 1) / (sp.exp(x) + 1)
        )
        == 0,
    )
    maximum_x = mp.findroot(lambda value: value * mp.tanh(value / 2) - 2, 2.4)
    expected_x = mp.mpf(
        "2.3993572805154676678327396972822838885229175768372"
    )
    verify("P81 numerical maximum", abs(maximum_x - expected_x) < mp.mpf("1e-49"))

    # P85
    beta = sp.symbols("beta", positive=True)
    energies = (sp.Integer(0), sp.Integer(1), sp.Integer(3))
    partition = sum(sp.exp(-beta * energy) for energy in energies)
    mean = -sp.diff(sp.log(partition), beta)
    second_moment = (
        sum(energy**2 * sp.exp(-beta * energy) for energy in energies) / partition
    )
    variance = sp.simplify(second_moment - mean**2)
    verify("P85 partition identity", sp.simplify(variance + sp.diff(mean, beta)) == 0)

    # P91
    proper_time, proper_acceleration, light_speed = sp.symbols(
        "proper_time proper_acceleration light_speed", positive=True
    )
    coordinate_time = light_speed / proper_acceleration * sp.sinh(
        proper_acceleration * proper_time / light_speed
    )
    position = light_speed**2 / proper_acceleration * (
        sp.cosh(proper_acceleration * proper_time / light_speed) - 1
    )
    velocity = light_speed * sp.tanh(
        proper_acceleration * proper_time / light_speed
    )
    verify(
        "P91 velocity",
        sp.simplify(
            sp.diff(position, proper_time) / sp.diff(coordinate_time, proper_time)
            - velocity
        )
        == 0,
    )
    verify(
        "P91 hyperbola",
        sp.simplify(
            (position + light_speed**2 / proper_acceleration) ** 2
            - light_speed**2 * coordinate_time**2
            - (light_speed**2 / proper_acceleration) ** 2
        )
        == 0,
    )
    verify(
        "P91 nonrelativistic limits",
        sp.limit(coordinate_time / proper_time, proper_time, 0) == 1
        and sp.limit(position / proper_time**2, proper_time, 0)
        == proper_acceleration / 2
        and sp.limit(velocity / proper_time, proper_time, 0) == proper_acceleration,
    )

    # P97: columns are F, R, U, rho, mu, c in base dimensions M, L, T.
    dimensions = sp.Matrix(
        [
            [1, 0, 0, 1, 1, 0],
            [1, 1, 1, -3, -1, 1],
            [-2, 0, -1, 0, -1, -1],
        ]
    )
    verify("P97 Pi count", len(dimensions.nullspace()) == 3)
    for name, vector in (
        ("drag coefficient", sp.Matrix([1, -2, -2, -1, 0, 0])),
        ("Reynolds number", sp.Matrix([0, 1, 1, 1, -1, 0])),
        ("Mach number", sp.Matrix([0, 0, 1, 0, 0, -1])),
    ):
        verify(f"P97 {name}", dimensions * vector == sp.zeros(3, 1))

    print(f"VERIFIED {len(PASSED)} assertions across all 20 problem IDs")
    for name in PASSED:
        print(f"PASS {name}")


if __name__ == "__main__":
    main()
