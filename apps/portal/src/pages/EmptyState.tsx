export function EmptyState({ title, message }: { title: string; message: string }) {
  return (
    <div className="page empty-state">
      <h1>{title}</h1>
      <p>{message}</p>
    </div>
  );
}
