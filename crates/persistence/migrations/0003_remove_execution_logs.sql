UPDATE ryvus_attempts
SET result = jsonb_set(
    result,
    '{events}',
    COALESCE(
        (
            SELECT jsonb_agg(event ORDER BY ordinal)
            FROM jsonb_array_elements(result -> 'events')
                 WITH ORDINALITY AS entries(event, ordinal)
            WHERE event ->> 'type' IS DISTINCT FROM 'log'
        ),
        '[]'::jsonb
    )
)
WHERE result IS NOT NULL
  AND jsonb_typeof(result -> 'events') = 'array'
  AND EXISTS (
      SELECT 1
      FROM jsonb_array_elements(result -> 'events') AS entries(event)
      WHERE event ->> 'type' = 'log'
  );
