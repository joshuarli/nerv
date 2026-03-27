"""Temperature converter with history tracking."""


class TemperatureConverter:
    def __init__(self):
        self.history = []

    def celsius_to_fahrenheit(self, c):
        result = c * 9 / 5 + 32
        self.history.append(("c_to_f", c, result))
        return result

    def fahrenheit_to_celsius(self, f):
        result = (f - 32) * 5 / 9
        self.history.append(("f_to_c", f, result))
        return result

    def celsius_to_kelvin(self, c):
        result = c + 273.15
        self.history.append(("c_to_k", c, result))
        return result

    def kelvin_to_celsius(self, k):
        result = k - 273.15
        self.history.append(("k_to_c", k, result))
        return result

    def get_history(self):
        return list(self.history)

    def clear_history(self):
        self.history.clear()
