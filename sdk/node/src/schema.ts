import type { JsonValue } from "./protocol.js";

export interface Schema<T = unknown> {
  jsonSchema: JsonValue;
  required: boolean;
  optional(): Schema<T | undefined>;
}

type Shape = Record<string, Schema>;

export type InferSchema<T> = T extends Schema<infer Value> ? Value : never;
export type InferShape<T extends Shape> = {
  [Key in keyof T as T[Key]["required"] extends false ? never : Key]: InferSchema<T[Key]>;
} & {
  [Key in keyof T as T[Key]["required"] extends false ? Key : never]?: InferSchema<T[Key]>;
};

export function string(): Schema<string> {
  return schema({ type: "string" });
}

export function number(): Schema<number> {
  return schema({ type: "number" });
}

export function integer(): Schema<number> {
  return schema({ type: "integer" });
}

export function boolean(): Schema<boolean> {
  return schema({ type: "boolean" });
}

export function array<Item extends Schema>(item: Item): Schema<InferSchema<Item>[]> {
  return schema({
    type: "array",
    items: item.jsonSchema,
  });
}

export function object<Fields extends Shape>(fields: Fields): Schema<InferShape<Fields>> {
  const properties: Record<string, JsonValue> = {};
  const required: string[] = [];

  for (const [name, field] of Object.entries(fields)) {
    properties[name] = field.jsonSchema;

    if (field.required) {
      required.push(name);
    }
  }

  return schema({
    type: "object",
    properties,
    ...(required.length > 0 ? { required } : {}),
  });
}

function schema<T>(jsonSchema: JsonValue, required = true): Schema<T> {
  return {
    jsonSchema,
    required,
    optional() {
      return schema<T | undefined>(jsonSchema, false);
    },
  };
}
